use crate::{AccountId, Availability, OnchainState, OutPoint, WalletScopeId};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObservationScope {
	/// The snapshot contains all UTXOs known to the source for the given accounts/scopes.
	/// UTXOs managed by txoscope but missing from this snapshot should be considered disappeared.
	Full,
	/// The snapshot contains only a subset of UTXOs (e.g., a single transaction or a delta).
	/// Missing UTXOs should be ignored.
	Incremental,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObservedConfirmation {
	Unknown,
	Mempool,
	Confirmed { height: u64, timestamp: u64 },
}

impl From<&ObservedConfirmation> for OnchainState {
	fn from(value: &ObservedConfirmation) -> Self {
		match value {
			ObservedConfirmation::Unknown => OnchainState::Unknown,
			ObservedConfirmation::Mempool => OnchainState::UnconfirmedIncoming,
			ObservedConfirmation::Confirmed { .. } => OnchainState::Confirmed,
		}
	}
}

impl From<&ObservedConfirmation> for Availability {
	fn from(value: &ObservedConfirmation) -> Self {
		match value {
			ObservedConfirmation::Confirmed { .. } => Availability::Selectable,
			ObservedConfirmation::Unknown | ObservedConfirmation::Mempool => {
				Availability::BlockedByPolicy
			},
		}
	}
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObservedSpendStatus {
	Unknown,
	Unspent,
	Spent {
		spending_txid: String,
	},
	/// The indexer explicitly reports this outpoint as no longer existing in its history.
	Missing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedUtxo {
	pub outpoint: OutPoint,
	pub account_id: AccountId,
	pub scope_id: WalletScopeId,
	pub value_sats: u64,
	pub confirmation: ObservedConfirmation,
	pub spend_status: ObservedSpendStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservationSnapshot {
	pub timestamp: u64,
	pub scope: ObservationScope,
	pub utxos: Vec<ObservedUtxo>,
}

impl ObservationSnapshot {
	pub fn new(timestamp: u64, scope: ObservationScope) -> Self {
		Self { timestamp, scope, utxos: Vec::new() }
	}

	pub fn add_utxo(mut self, utxo: ObservedUtxo) -> Self {
		self.utxos.push(utxo);
		self
	}
}

impl ObservedUtxo {
	pub fn new(
		outpoint: OutPoint, account_id: impl Into<AccountId>, scope_id: impl Into<WalletScopeId>,
		value_sats: u64,
	) -> Self {
		Self {
			outpoint,
			account_id: account_id.into(),
			scope_id: scope_id.into(),
			value_sats,
			confirmation: ObservedConfirmation::Unknown,
			spend_status: ObservedSpendStatus::Unspent,
		}
	}

	pub fn confirmed(mut self, height: u64, timestamp: u64) -> Self {
		self.confirmation = ObservedConfirmation::Confirmed { height, timestamp };
		self
	}

	pub fn mempool(mut self) -> Self {
		self.confirmation = ObservedConfirmation::Mempool;
		self
	}

	pub fn spent(mut self, spending_txid: impl Into<String>) -> Self {
		self.spend_status = ObservedSpendStatus::Spent { spending_txid: spending_txid.into() };
		self
	}
}

pub trait ObservationSource {
	type Error: std::error::Error + Send + Sync + 'static;
	fn collect_snapshot(&self) -> Result<ObservationSnapshot, Self::Error>;
}
