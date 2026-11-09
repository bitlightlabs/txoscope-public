use crate::{AccountId, Capability, CapabilitySet, OperationId, WalletScopeId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::EnumString, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
#[cfg_attr(test, derive(strum::EnumIter))]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "schema", schema(rename_all = "snake_case"))]
pub enum LockPurpose {
	ManualReservation,
	BtcSend,
	/// A planned allocation-free input supplying bitcoin fees.
	FeeSupport,
	PlannedSelection,
}

impl LockPurpose {
	pub(crate) fn required_capabilities(self) -> CapabilitySet {
		match self {
			Self::BtcSend => CapabilitySet::new([Capability::BtcSpend]),
			Self::PlannedSelection => CapabilitySet::new([Capability::ReserveSupport]),
			Self::FeeSupport => {
				CapabilitySet::new([Capability::ReserveSupport, Capability::FeeSupport])
			},
			Self::ManualReservation => {
				CapabilitySet::new([Capability::ReserveSupport, Capability::SingleTargetReserve])
			},
		}
	}

	pub(crate) fn required_change_capabilities(self) -> CapabilitySet {
		// Fee support is an input role; the resulting change remains a planned output.
		match self {
			Self::FeeSupport => Self::PlannedSelection.required_capabilities(),
			other => other.required_capabilities(),
		}
	}
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationLock {
	pub operation_id: OperationId,
	pub account_id: AccountId,
	pub scope_id: WalletScopeId,
	pub purpose: LockPurpose,
	pub owner: String,
	pub created_at: u64,
	pub expires_at: Option<u64>,
}
