use crate::{
	AccountId, Availability, ObservedConfirmation, OnchainState, OperationBinding, OperationId,
	OperationPhase, UtxoRole, WalletScopeId,
};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OutPoint {
	pub txid: String,
	pub vout: u32,
}

impl OutPoint {
	pub fn new(txid: impl Into<String>, vout: u32) -> Self {
		Self { txid: txid.into(), vout }
	}
}

impl core::fmt::Display for OutPoint {
	fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
		write!(f, "{}:{}", self.txid, self.vout)
	}
}

impl core::str::FromStr for OutPoint {
	type Err = crate::DomainError;

	/// Parses the last colon as the separator and preserves the opaque transaction ID.
	fn from_str(value: &str) -> Result<Self, Self::Err> {
		let invalid = || {
			crate::DomainError::InvalidRequest {
            reason: format!("invalid outpoint {value:?}: expected a transaction ID followed by ':' and a u32 output index"),
        }
		};
		let (txid, vout) = value.rsplit_once(':').ok_or_else(invalid)?;
		let vout = vout.parse::<u32>().map_err(|_| invalid())?;
		Ok(Self::new(txid, vout))
	}
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservationConflict {
	pub message: String,
	pub recorded_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedUtxo {
	pub outpoint: OutPoint,
	pub account_id: AccountId,
	pub scope_id: WalletScopeId,
	pub value_sats: u64,
	pub onchain_state: OnchainState,
	pub availability: Availability,
	pub role: UtxoRole,
	pub operation_binding: Option<OperationBinding>,
	pub observation_conflict: Option<ObservationConflict>,
}

impl ManagedUtxo {
	pub fn new(
		outpoint: OutPoint, account_id: impl Into<AccountId>, scope_id: impl Into<WalletScopeId>,
		value_sats: u64,
	) -> Self {
		Self {
			outpoint,
			account_id: account_id.into(),
			scope_id: scope_id.into(),
			value_sats,
			onchain_state: OnchainState::Confirmed,
			availability: Availability::Selectable,
			role: UtxoRole::General,
			operation_binding: None,
			observation_conflict: None,
		}
	}

	pub fn is_policy_spendable(&self) -> bool {
		self.availability == Availability::Selectable
			&& self.onchain_state == OnchainState::Confirmed
			&& self.operation_binding.is_none()
			&& self.observation_conflict.is_none()
	}

	pub fn can_be_locked(&self, lock_present: bool) -> bool {
		self.is_policy_spendable() && !lock_present
	}

	pub fn reserve(&mut self, operation_id: impl Into<OperationId>, phase: OperationPhase) {
		self.availability = Availability::Reserved;
		self.operation_binding =
			Some(OperationBinding { operation_id: operation_id.into(), phase });
	}

	pub fn discover_from_observed(
		outpoint: OutPoint, account_id: impl Into<AccountId>, scope_id: impl Into<WalletScopeId>,
		value_sats: u64, confirmation: &ObservedConfirmation,
	) -> Self {
		Self {
			outpoint,
			account_id: account_id.into(),
			scope_id: scope_id.into(),
			value_sats,
			onchain_state: confirmation.into(),
			availability: confirmation.into(),
			role: UtxoRole::General,
			operation_binding: None,
			observation_conflict: None,
		}
	}

	pub fn mark_spent(&mut self) {
		self.onchain_state = OnchainState::Spent;
		self.availability = Availability::BlockedByL2;
		self.operation_binding = None;
	}

	pub fn mark_missing(&mut self) {
		self.availability = Availability::Recovering;
	}

	pub fn release_binding(&mut self) {
		if matches!(self.onchain_state, OnchainState::Spent | OnchainState::SpendingInFlight) {
			self.availability = Availability::BlockedByL2;
		} else if self.availability != Availability::Recovering {
			self.availability = if self.onchain_state == OnchainState::Confirmed
				&& self.observation_conflict.is_none()
			{
				Availability::Selectable
			} else {
				Availability::BlockedByPolicy
			};
		}
		self.operation_binding = None;
	}

	/// Advances local ownership without releasing the input. Broadcasting (or
	/// skipping directly to a later phase) starts spending; constructing and
	/// signing alone provide no new on-chain information.
	pub fn advance_binding(&mut self, phase: OperationPhase) {
		let Some(binding) = self.operation_binding.as_mut() else {
			return;
		};
		binding.phase = phase;
		if matches!(
			phase,
			OperationPhase::Broadcast
				| OperationPhase::AwaitingConfirmation
				| OperationPhase::AwaitingL2Finality
		) {
			if self.onchain_state != OnchainState::Spent {
				self.onchain_state = OnchainState::SpendingInFlight;
			}
			self.availability = Availability::BlockedByL2;
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::ObservationConflict;

	#[test]
	fn outpoints_round_trip_opaque_transaction_ids() {
		for txid in ["", " ", "交易:opaque:tx", "txid", "x:y:"] {
			for vout in [0, 2, 10, u32::MAX] {
				let outpoint = OutPoint::new(txid, vout);
				assert_eq!(outpoint.to_string().parse::<OutPoint>().unwrap(), outpoint);
			}
		}
	}

	#[test]
	fn outpoint_parse_rejects_missing_or_invalid_output_index() {
		for value in ["", "txid", "txid:", "txid:-1", "txid:4294967296", "txid:no", "txid: 1"] {
			assert!(matches!(
				value.parse::<OutPoint>(),
				Err(crate::DomainError::InvalidRequest { .. })
			));
		}
	}

	#[test]
	fn discovered_utxos_follow_confirmation_spendability_rules() {
		let confirmed = ManagedUtxo::discover_from_observed(
			OutPoint::new("tx-confirmed", 0),
			"acct",
			"scope",
			1_000,
			&ObservedConfirmation::Confirmed { height: 1, timestamp: 2 },
		);
		assert!(confirmed.is_policy_spendable());
		assert!(confirmed.can_be_locked(false));

		let mempool = ManagedUtxo::discover_from_observed(
			OutPoint::new("tx-mempool", 0),
			"acct",
			"scope",
			1_000,
			&ObservedConfirmation::Mempool,
		);
		assert_eq!(mempool.onchain_state, OnchainState::UnconfirmedIncoming);
		assert!(!mempool.is_policy_spendable());
		assert!(!mempool.can_be_locked(false));

		let unknown = ManagedUtxo::discover_from_observed(
			OutPoint::new("tx-unknown", 0),
			"acct",
			"scope",
			1_000,
			&ObservedConfirmation::Unknown,
		);
		assert_eq!(unknown.onchain_state, OnchainState::Unknown);
		assert!(!unknown.is_policy_spendable());
		assert!(!unknown.can_be_locked(false));
	}

	#[test]
	fn reserve_mark_spent_and_mark_missing_follow_core_rules() {
		let mut utxo = ManagedUtxo::new(OutPoint::new("tx", 0), "acct", "scope", 1_000);
		assert!(utxo.can_be_locked(false));

		utxo.reserve("op1", OperationPhase::Selected);
		assert_eq!(utxo.availability, Availability::Reserved);
		assert_eq!(utxo.operation_binding.as_ref().unwrap().operation_id, "op1".into());

		utxo.mark_missing();
		assert_eq!(utxo.availability, Availability::Recovering);

		utxo.mark_spent();
		assert_eq!(utxo.onchain_state, OnchainState::Spent);
		assert_eq!(utxo.availability, Availability::BlockedByL2);
		assert!(utxo.operation_binding.is_none());
	}

	#[test]
	fn release_binding_restores_selectable_or_policy_blocked() {
		let mut healthy = ManagedUtxo::new(OutPoint::new("tx-healthy", 0), "acct", "scope", 1_000);
		healthy.reserve("op1", OperationPhase::Selected);
		healthy.release_binding();
		assert_eq!(healthy.availability, Availability::Selectable);
		assert!(healthy.operation_binding.is_none());

		let mut conflicted =
			ManagedUtxo::new(OutPoint::new("tx-conflict", 0), "acct", "scope", 1_000);
		conflicted.reserve("op1", OperationPhase::Selected);
		conflicted.observation_conflict =
			Some(ObservationConflict { message: "mismatch".to_string(), recorded_at: 1 });
		conflicted.release_binding();
		assert_eq!(conflicted.availability, Availability::BlockedByPolicy);
		assert!(conflicted.operation_binding.is_none());

		let mut recovering =
			ManagedUtxo::new(OutPoint::new("tx-recovering", 0), "acct", "scope", 1_000);
		recovering.availability = Availability::Recovering;
		recovering.reserve("op1", OperationPhase::Selected);
		recovering.mark_missing();
		recovering.release_binding();
		assert_eq!(recovering.availability, Availability::Recovering);
	}

	#[test]
	fn advancing_binding_retains_ownership_until_observed_spent() {
		let mut utxo = ManagedUtxo::new(OutPoint::new("tx-complete", 0), "acct", "scope", 1_000);
		utxo.reserve("op1", OperationPhase::Selected);
		for phase in [OperationPhase::Constructing, OperationPhase::Signed] {
			utxo.advance_binding(phase);
			assert_eq!(utxo.onchain_state, OnchainState::Confirmed);
			assert_eq!(utxo.availability, Availability::Reserved);
			assert_eq!(utxo.operation_binding.as_ref().unwrap().phase, phase);
			assert!(!utxo.is_policy_spendable());
		}
		utxo.advance_binding(OperationPhase::Broadcast);
		assert_eq!(utxo.onchain_state, OnchainState::SpendingInFlight);
		assert_eq!(utxo.availability, Availability::BlockedByL2);
		assert_eq!(utxo.operation_binding.as_ref().unwrap().phase, OperationPhase::Broadcast);

		utxo.mark_spent();
		utxo.advance_binding(OperationPhase::AwaitingConfirmation);
		assert_eq!(utxo.onchain_state, OnchainState::Spent);
		assert_eq!(utxo.availability, Availability::BlockedByL2);
		assert!(utxo.operation_binding.is_none());
	}
}
