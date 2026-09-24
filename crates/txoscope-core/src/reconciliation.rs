use crate::{
	errors::DomainResult, observation::ObservationScope, store::StateRead, Availability,
	ObservationSnapshot, ObservedSpendStatus, ObservedUtxo, OnchainState, OutPoint,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReconciliationAction {
	DiscoverNew(ObservedUtxo),
	RefreshObserved(ObservedUtxo),
	UpdateOnchainState { outpoint: OutPoint, new_state: OnchainState },
	MarkSpent { outpoint: OutPoint, spending_txid: String },
	ReportConflict { outpoint: OutPoint, message: String, recorded_at: u64 },
	ClearConflict(OutPoint),
	UpdateAvailability { outpoint: OutPoint, availability: Availability },
	MarkMissing(OutPoint),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconcilePlan {
	pub actions: Vec<ReconciliationAction>,
}

pub struct ReconciliationPlanner<'a> {
	state: &'a dyn StateRead,
}

impl<'a> ReconciliationPlanner<'a> {
	pub fn new(state: &'a dyn StateRead) -> Self {
		Self { state }
	}

	fn observed_mismatch_message(
		managed: &crate::ManagedUtxo, observed: &ObservedUtxo,
	) -> Option<String> {
		let mut mismatches = Vec::new();
		if managed.value_sats != observed.value_sats {
			mismatches.push(format!(
				"value_sats(local={}, observed={})",
				managed.value_sats, observed.value_sats
			));
		}
		if managed.account_id != observed.account_id {
			mismatches.push(format!(
				"account_id(local={}, observed={})",
				managed.account_id, observed.account_id
			));
		}
		if managed.scope_id != observed.scope_id {
			mismatches.push(format!(
				"scope_id(local={}, observed={})",
				managed.scope_id, observed.scope_id
			));
		}
		if mismatches.is_empty() {
			None
		} else {
			Some(format!("Observed metadata mismatch: {}", mismatches.join(", ")))
		}
	}

	fn restored_unspent_availability(
		managed: &crate::ManagedUtxo, observed: &ObservedUtxo, has_mismatch: bool,
	) -> Option<Availability> {
		if managed.onchain_state == OnchainState::Spent {
			return None;
		}
		// Unspent observations can lag a local broadcast. This also protects
		// legacy in-flight rows whose operation binding was already removed.
		if managed.onchain_state == OnchainState::SpendingInFlight {
			return (managed.availability != Availability::BlockedByL2)
				.then_some(Availability::BlockedByL2);
		}
		if managed.operation_binding.is_some() {
			return (managed.availability == Availability::Recovering)
				.then_some(Availability::Reserved);
		}
		if has_mismatch {
			return (managed.availability != Availability::BlockedByPolicy)
				.then_some(Availability::BlockedByPolicy);
		}

		let desired = Availability::from(&observed.confirmation);
		let can_restore = managed.availability == Availability::Recovering
			|| managed.availability == Availability::BlockedByPolicy
			|| managed.availability == Availability::Selectable;

		(can_restore && managed.availability != desired).then_some(desired)
	}

	pub fn plan(&self, snapshot: &ObservationSnapshot) -> DomainResult<ReconcilePlan> {
		let state = self.state;
		let mut actions = Vec::new();
		let managed_utxos = state.list_utxos()?;

		// Check for new or updated UTXOs from snapshot
		for observed in &snapshot.utxos {
			let managed = managed_utxos.iter().find(|u| u.outpoint == observed.outpoint);

			match managed {
				None => {
					// If unspent, discover it
					if let ObservedSpendStatus::Unspent = observed.spend_status {
						actions.push(ReconciliationAction::DiscoverNew(observed.clone()));
					}
				},
				Some(managed) => {
					// Check for spend status changes
					match &observed.spend_status {
						ObservedSpendStatus::Spent { spending_txid } => {
							if managed.onchain_state != OnchainState::Spent {
								if managed.availability == Availability::Reserved {
									actions.push(ReconciliationAction::ReportConflict {
										outpoint: managed.outpoint.clone(),
										message: format!(
											"Reserved UTXO spent by {}",
											spending_txid
										),
										recorded_at: snapshot.timestamp,
									});
								}
								actions.push(ReconciliationAction::MarkSpent {
									outpoint: managed.outpoint.clone(),
									spending_txid: spending_txid.clone(),
								});
							}
						},
						ObservedSpendStatus::Missing => {
							if managed.onchain_state != OnchainState::Spent {
								if managed.availability == Availability::Reserved {
									actions.push(ReconciliationAction::ReportConflict {
										outpoint: managed.outpoint.clone(),
										message: "Reserved UTXO explicitly reported as missing"
											.to_string(),
										recorded_at: snapshot.timestamp,
									});
								}
								actions.push(ReconciliationAction::MarkMissing(
									managed.outpoint.clone(),
								));
							}
						},
						ObservedSpendStatus::Unspent => {
							if managed.onchain_state == OnchainState::Spent {
								continue;
							}

							let mismatch = Self::observed_mismatch_message(managed, observed);
							if let Some(message) = mismatch.as_ref() {
								actions.push(ReconciliationAction::ReportConflict {
									outpoint: managed.outpoint.clone(),
									message: message.clone(),
									recorded_at: snapshot.timestamp,
								});
							}

							if let Some(availability) = Self::restored_unspent_availability(
								managed,
								observed,
								mismatch.is_some(),
							) {
								actions.push(ReconciliationAction::UpdateAvailability {
									outpoint: managed.outpoint.clone(),
									availability,
								});
							}

							if managed.observation_conflict.is_some() && mismatch.is_none() {
								actions.push(ReconciliationAction::ClearConflict(
									managed.outpoint.clone(),
								));
							}

							// Check for confirmation status updates
							let new_state = OnchainState::from(&observed.confirmation);

							if managed.onchain_state != OnchainState::SpendingInFlight
								&& managed.onchain_state != new_state
							{
								actions.push(ReconciliationAction::UpdateOnchainState {
									outpoint: managed.outpoint.clone(),
									new_state,
								});
							}
						},
						ObservedSpendStatus::Unknown => {
							// If spend status is unknown, we can't reliably restore or update onchain_state
						},
					}
				},
			}
		}

		// Check for managed UTXOs missing from snapshot if the snapshot is FULL
		if snapshot.scope == ObservationScope::Full {
			for managed in managed_utxos {
				if managed.onchain_state != OnchainState::Spent
					&& managed.availability != Availability::Recovering
					&& managed.onchain_state != OnchainState::SpendingInFlight
				{
					let in_snapshot = snapshot.utxos.iter().any(|o| o.outpoint == managed.outpoint);
					if !in_snapshot {
						if managed.availability == Availability::Reserved {
							actions.push(ReconciliationAction::ReportConflict {
								outpoint: managed.outpoint.clone(),
								message: "Reserved UTXO missing from full snapshot".to_string(),
								recorded_at: snapshot.timestamp,
							});
						}
						actions.push(ReconciliationAction::MarkMissing(managed.outpoint.clone()));
					}
				}
			}
		}

		Ok(ReconcilePlan { actions })
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::{
		lock::OperationLock,
		operation::{OperationIntent, PersistedOperation},
		Account, ManagedUtxo, ObservationScope, ObservedUtxo, WalletScope,
	};
	use crate::{AccountId, OperationId, WalletScopeId};

	struct StaticState {
		utxos: Vec<ManagedUtxo>,
	}

	impl StateRead for StaticState {
		fn get_account(&self, _account_id: &AccountId) -> DomainResult<Option<Account>> {
			Ok(None)
		}

		fn get_scope(&self, _scope_id: &WalletScopeId) -> DomainResult<Option<WalletScope>> {
			Ok(None)
		}

		fn get_utxo(&self, outpoint: &OutPoint) -> DomainResult<Option<ManagedUtxo>> {
			Ok(self.utxos.iter().find(|utxo| &utxo.outpoint == outpoint).cloned())
		}

		fn get_lock(&self, _outpoint: &OutPoint) -> DomainResult<Option<OperationLock>> {
			Ok(None)
		}

		fn get_operation(
			&self, _operation_id: &OperationId,
		) -> DomainResult<Option<PersistedOperation>> {
			Ok(None)
		}

		fn get_intent(&self, _operation_id: &OperationId) -> DomainResult<Option<OperationIntent>> {
			Ok(None)
		}

		fn list_utxos(&self) -> DomainResult<Vec<ManagedUtxo>> {
			Ok(self.utxos.clone())
		}

		fn list_operations(&self) -> DomainResult<Vec<PersistedOperation>> {
			Ok(Vec::new())
		}

		fn list_pending_intents(&self) -> DomainResult<Vec<OperationIntent>> {
			Ok(Vec::new())
		}

		fn current_tick(&self) -> DomainResult<u64> {
			Ok(0)
		}
	}

	#[test]
	fn mempool_observation_restores_policy_block_and_unconfirmed_state() {
		let outpoint = OutPoint::new("tx-mempool", 0);
		let state =
			StaticState { utxos: vec![ManagedUtxo::new(outpoint.clone(), "acct", "scope", 1_000)] };
		let snapshot = ObservationSnapshot::new(42, ObservationScope::Incremental)
			.add_utxo(ObservedUtxo::new(outpoint.clone(), "acct", "scope", 1_000).mempool());

		let plan = ReconciliationPlanner::new(&state).plan(&snapshot).unwrap();

		assert!(plan.actions.contains(&ReconciliationAction::UpdateAvailability {
			outpoint: outpoint.clone(),
			availability: Availability::BlockedByPolicy,
		}));
		assert!(plan.actions.contains(&ReconciliationAction::UpdateOnchainState {
			outpoint,
			new_state: OnchainState::UnconfirmedIncoming,
		}));
	}

	#[test]
	fn spent_utxos_do_not_reopen_on_unspent_observation() {
		let outpoint = OutPoint::new("tx-spent", 0);
		let mut spent = ManagedUtxo::new(outpoint.clone(), "acct", "scope", 1_000);
		spent.mark_spent();
		let state = StaticState { utxos: vec![spent] };
		let snapshot = ObservationSnapshot::new(43, ObservationScope::Incremental).add_utxo(
			ObservedUtxo::new(outpoint.clone(), "acct", "scope", 1_000).confirmed(101, 43),
		);

		let plan = ReconciliationPlanner::new(&state).plan(&snapshot).unwrap();

		assert!(!plan.actions.iter().any(|action| {
			matches!(
				action,
				ReconciliationAction::UpdateOnchainState { outpoint: candidate, .. }
					if candidate == &outpoint
			)
		}));
		assert!(!plan.actions.iter().any(|action| {
			matches!(
				action,
				ReconciliationAction::UpdateAvailability { outpoint: candidate, .. }
					if candidate == &outpoint
			)
		}));
	}

	#[test]
	fn spent_utxos_keep_existing_conflicts_on_unspent_observation() {
		let outpoint = OutPoint::new("tx-spent-conflict", 0);
		let mut spent = ManagedUtxo::new(outpoint.clone(), "acct", "scope", 1_000);
		spent.mark_spent();
		spent.observation_conflict = Some(crate::ObservationConflict {
			message: "reserved utxo spent by tx-ext".to_string(),
			recorded_at: 42,
		});
		let state = StaticState { utxos: vec![spent] };
		let snapshot = ObservationSnapshot::new(44, ObservationScope::Incremental).add_utxo(
			ObservedUtxo::new(outpoint.clone(), "acct", "scope", 1_000).confirmed(101, 44),
		);

		let plan = ReconciliationPlanner::new(&state).plan(&snapshot).unwrap();

		assert!(!plan.actions.iter().any(|action| {
			matches!(action, ReconciliationAction::ClearConflict(candidate) if candidate == &outpoint)
		}));
	}
}
