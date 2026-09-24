use crate::{
	lock::LockPurpose,
	operation::{
		InputSelectionRequest, InputSelectionSpec, OperationPurpose, OperationRequest,
		SelectionLockPolicy,
	},
	AccountId, OperationId, OutPoint,
};

/// Capabilities are required on both the account and each selected scope.
/// BTC sends require `BtcSpend`; planned selections require `ReserveSupport`;
/// manual reservations require both `ReserveSupport` and `SingleTargetReserve`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SelectionRequest {
	BtcSend(BtcSendRequest),
	PlannedExactOutpoints(PlannedExactOutpointsRequest),
	PlannedSpecificOutpoint(PlannedSpecificOutpointRequest),
	ManualReservationExact(ManualReservationExactRequest),
	ManualReservationFirstAvailable(ManualReservationFirstAvailableRequest),
}

impl SelectionRequest {
	pub(crate) fn into_operation_request(self) -> OperationRequest {
		match self {
			Self::BtcSend(req) => OperationRequest::new(
				req.operation_id,
				InputSelectionRequest::new(
					req.account_id,
					InputSelectionSpec::TargetValue { amount_sats: req.amount_sats },
					LockPurpose::BtcSend.required_capabilities(),
				),
				SelectionLockPolicy::new(OperationPurpose::BtcSend, None),
			),
			Self::PlannedExactOutpoints(req) => OperationRequest::new(
				req.operation_id,
				InputSelectionRequest::new(
					req.account_id,
					InputSelectionSpec::ExactOutpoints { outpoints: req.outpoints },
					LockPurpose::PlannedSelection.required_capabilities(),
				),
				SelectionLockPolicy::new(OperationPurpose::PlannedSelection, None),
			),
			Self::PlannedSpecificOutpoint(req) => OperationRequest::new(
				req.operation_id,
				InputSelectionRequest::new(
					req.account_id,
					InputSelectionSpec::ExactOutpoints { outpoints: vec![req.outpoint] },
					LockPurpose::PlannedSelection.required_capabilities(),
				),
				SelectionLockPolicy::new(OperationPurpose::PlannedSelection, req.expires_at),
			),
			Self::ManualReservationExact(req) => OperationRequest::new(
				req.reservation_id,
				InputSelectionRequest::new(
					req.account_id,
					InputSelectionSpec::ExactOutpoints { outpoints: vec![req.outpoint] },
					LockPurpose::ManualReservation.required_capabilities(),
				),
				SelectionLockPolicy::new(OperationPurpose::ManualReservation, Some(req.expires_at)),
			),
			Self::ManualReservationFirstAvailable(req) => {
				let mut candidates = req.candidate_outpoints;
				candidates.sort_by_key(ToString::to_string);
				OperationRequest::new(
					req.reservation_id,
					InputSelectionRequest::new(
						req.account_id,
						InputSelectionSpec::FirstSelectable { candidates },
						LockPurpose::ManualReservation.required_capabilities(),
					),
					SelectionLockPolicy::new(
						OperationPurpose::ManualReservation,
						Some(req.expires_at),
					),
				)
			},
		}
	}
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct BtcSendRequest {
	operation_id: OperationId,
	account_id: AccountId,
	amount_sats: u64,
}

impl BtcSendRequest {
	pub fn new(
		operation_id: impl Into<OperationId>, account_id: impl Into<AccountId>, amount_sats: u64,
	) -> Self {
		Self { operation_id: operation_id.into(), account_id: account_id.into(), amount_sats }
	}
}

impl From<BtcSendRequest> for SelectionRequest {
	fn from(value: BtcSendRequest) -> Self {
		Self::BtcSend(value)
	}
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct PlannedExactOutpointsRequest {
	operation_id: OperationId,
	account_id: AccountId,
	outpoints: Vec<OutPoint>,
}

impl PlannedExactOutpointsRequest {
	pub fn new(
		operation_id: impl Into<OperationId>, account_id: impl Into<AccountId>,
		outpoints: Vec<OutPoint>,
	) -> Self {
		Self { operation_id: operation_id.into(), account_id: account_id.into(), outpoints }
	}
}

impl From<PlannedExactOutpointsRequest> for SelectionRequest {
	fn from(value: PlannedExactOutpointsRequest) -> Self {
		Self::PlannedExactOutpoints(value)
	}
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct PlannedSpecificOutpointRequest {
	operation_id: OperationId,
	account_id: AccountId,
	outpoint: OutPoint,
	expires_at: Option<u64>,
}

impl PlannedSpecificOutpointRequest {
	pub fn new(
		operation_id: impl Into<OperationId>, account_id: impl Into<AccountId>, outpoint: OutPoint,
	) -> Self {
		Self {
			operation_id: operation_id.into(),
			account_id: account_id.into(),
			outpoint,
			expires_at: None,
		}
	}

	pub fn expires_at(mut self, expires_at: u64) -> Self {
		self.expires_at = Some(expires_at);
		self
	}
}

impl From<PlannedSpecificOutpointRequest> for SelectionRequest {
	fn from(value: PlannedSpecificOutpointRequest) -> Self {
		Self::PlannedSpecificOutpoint(value)
	}
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ManualReservationExactRequest {
	reservation_id: OperationId,
	account_id: AccountId,
	outpoint: OutPoint,
	expires_at: u64,
}

impl ManualReservationExactRequest {
	pub fn new(
		reservation_id: impl Into<OperationId>, account_id: impl Into<AccountId>,
		outpoint: OutPoint, expires_at: u64,
	) -> Self {
		Self {
			reservation_id: reservation_id.into(),
			account_id: account_id.into(),
			outpoint,
			expires_at,
		}
	}
}

impl From<ManualReservationExactRequest> for SelectionRequest {
	fn from(value: ManualReservationExactRequest) -> Self {
		Self::ManualReservationExact(value)
	}
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ManualReservationFirstAvailableRequest {
	reservation_id: OperationId,
	account_id: AccountId,
	candidate_outpoints: Vec<OutPoint>,
	expires_at: u64,
}

impl ManualReservationFirstAvailableRequest {
	pub fn new(
		reservation_id: impl Into<OperationId>, account_id: impl Into<AccountId>,
		candidate_outpoints: Vec<OutPoint>, expires_at: u64,
	) -> Self {
		Self {
			reservation_id: reservation_id.into(),
			account_id: account_id.into(),
			candidate_outpoints,
			expires_at,
		}
	}
}

impl From<ManualReservationFirstAvailableRequest> for SelectionRequest {
	fn from(value: ManualReservationFirstAvailableRequest) -> Self {
		Self::ManualReservationFirstAvailable(value)
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::{operation::InputSelectionSpec, Capability, CapabilitySet};

	#[test]
	fn btc_send_request_derives_btc_send_locking_and_capabilities() {
		let request = SelectionRequest::from(BtcSendRequest::new("op-btc", "btc", 50_000))
			.into_operation_request();

		assert_eq!(request.operation_id, "op-btc".into());
		assert_eq!(request.selection.account_id, "btc".into());
		assert_eq!(
			request.selection.required_capabilities,
			CapabilitySet::new([Capability::BtcSpend])
		);
		assert_eq!(request.locking.lock_purpose(), LockPurpose::BtcSend);
		assert_eq!(request.locking.expires_at, None);
		assert_eq!(request.selection.spec, InputSelectionSpec::TargetValue { amount_sats: 50_000 });
	}

	#[test]
	fn planned_specific_outpoint_request_keeps_optional_expiry() {
		let request = SelectionRequest::from(
			PlannedSpecificOutpointRequest::new("op-plan", "rgb", OutPoint::new("txid", 1))
				.expires_at(42),
		)
		.into_operation_request();

		assert_eq!(request.operation_id, "op-plan".into());
		assert_eq!(request.selection.account_id, "rgb".into());
		assert_eq!(request.locking.lock_purpose(), LockPurpose::PlannedSelection);
		assert_eq!(request.locking.expires_at, Some(42));
		assert_eq!(
			request.selection.spec,
			InputSelectionSpec::ExactOutpoints { outpoints: vec![OutPoint::new("txid", 1)] }
		);
	}

	#[test]
	fn manual_candidate_order_preserves_serialized_lexical_order() {
		let request = SelectionRequest::from(ManualReservationFirstAvailableRequest::new(
			"manual",
			"account",
			vec![OutPoint::new("x", 2), OutPoint::new("x:", 0), OutPoint::new("x", 10)],
			42,
		))
		.into_operation_request();
		assert_eq!(
			request.selection.spec,
			InputSelectionSpec::FirstSelectable {
				candidates: vec![
					OutPoint::new("x", 10),
					OutPoint::new("x", 2),
					OutPoint::new("x:", 0)
				],
			}
		);
	}

	#[test]
	fn manual_first_available_request_sorts_candidates_and_sets_expiry() {
		let request = SelectionRequest::from(ManualReservationFirstAvailableRequest::new(
			"manual-1",
			"rgb",
			vec![OutPoint::new("tx-b", 1), OutPoint::new("tx-a", 0)],
			1_234,
		))
		.into_operation_request();

		assert_eq!(request.operation_id, "manual-1".into());
		assert_eq!(request.selection.account_id, "rgb".into());
		assert_eq!(request.locking.lock_purpose(), LockPurpose::ManualReservation);
		assert_eq!(request.locking.expires_at, Some(1_234));
		assert_eq!(
			request.selection.spec,
			InputSelectionSpec::FirstSelectable {
				candidates: vec![OutPoint::new("tx-a", 0), OutPoint::new("tx-b", 1)],
			}
		);
	}
}
