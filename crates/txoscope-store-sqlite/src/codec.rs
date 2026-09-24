use std::io;

use txoscope_core::{Capability, CapabilitySet, DomainError, OutPoint};

#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct SqliteCodec;

impl SqliteCodec {
	pub(crate) fn map_sql_error(err: rusqlite::Error) -> DomainError {
		match err {
			rusqlite::Error::FromSqlConversionFailure(_, _, source) => {
				DomainError::CorruptedState(source.to_string())
			},
			err @ rusqlite::Error::InvalidColumnType(_, _, _)
			| err @ rusqlite::Error::IntegralValueOutOfRange(_, _) => {
				DomainError::CorruptedState(err.to_string())
			},
			other => DomainError::Storage(other.to_string()),
		}
	}

	pub(crate) fn corrupted_state_error(message: impl Into<String>) -> rusqlite::Error {
		rusqlite::Error::FromSqlConversionFailure(
			0,
			rusqlite::types::Type::Text,
			Box::new(io::Error::new(io::ErrorKind::InvalidData, message.into())),
		)
	}

	pub(crate) fn encode<T>(value: T) -> &'static str
	where
		T: Into<&'static str>,
	{
		value.into()
	}

	pub(crate) fn decode<T>(encoded: &str) -> rusqlite::Result<T>
	where
		T: core::str::FromStr,
	{
		encoded.parse().map_err(|_| {
			Self::corrupted_state_error(format!(
				"invalid {} value in sqlite row: {encoded}",
				core::any::type_name::<T>()
			))
		})
	}

	pub(crate) fn decode_outpoint(encoded: &str) -> rusqlite::Result<OutPoint> {
		let outpoint: OutPoint = encoded.parse().map_err(|error| {
			Self::corrupted_state_error(format!(
				"invalid outpoint in sqlite row: {encoded}: {error}"
			))
		})?;
		if outpoint.to_string() != encoded {
			return Err(Self::corrupted_state_error(format!(
				"noncanonical outpoint in sqlite row: {encoded}"
			)));
		}
		Ok(outpoint)
	}

	pub(crate) fn encode_capabilities(capabilities: &CapabilitySet) -> String {
		capabilities
			.iter()
			.map(|capability| Self::encode(*capability))
			.collect::<Vec<_>>()
			.join(",")
	}

	pub(crate) fn decode_capabilities(encoded: &str) -> rusqlite::Result<CapabilitySet> {
		let capabilities = if encoded.is_empty() {
			Vec::new()
		} else {
			encoded
				.split(',')
				.map(Self::decode::<Capability>)
				.collect::<rusqlite::Result<Vec<_>>>()?
		};
		Ok(CapabilitySet::new(capabilities))
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use txoscope_core::{
		backend::{IntentStatus, LockPurpose},
		Availability, OnchainState, OperationPhase, UtxoRole,
	};

	#[test]
	fn capability_codec_round_trips_current_names() {
		for (capability, encoded) in [
			(Capability::BtcSpend, "btc_spend"),
			(Capability::BtcReceive, "btc_receive"),
			(Capability::FeeSupport, "fee_support"),
			(Capability::ReserveSupport, "reserve_support"),
			(Capability::SingleTargetReserve, "single_target_reserve"),
			(Capability::L2Settlement, "l2_settlement"),
			(Capability::ConsolidationSource, "consolidation_source"),
			(Capability::ConsolidationTarget, "consolidation_target"),
		] {
			assert_eq!(SqliteCodec::encode(capability), encoded);
			assert_eq!(SqliteCodec::decode::<Capability>(encoded).unwrap(), capability);
		}

		let capabilities = CapabilitySet::new([
			Capability::BtcSpend,
			Capability::SingleTargetReserve,
			Capability::L2Settlement,
		]);

		let encoded = SqliteCodec::encode_capabilities(&capabilities);
		let decoded = SqliteCodec::decode_capabilities(&encoded).unwrap();

		assert_eq!(decoded, capabilities);
	}

	#[test]
	fn capability_codec_preserves_legacy_names() {
		assert_eq!(
			SqliteCodec::decode::<Capability>("rgb_receive").unwrap(),
			Capability::SingleTargetReserve
		);
	}

	#[test]
	fn state_codec_round_trips_current_names() {
		for (value, encoded) in [
			(OnchainState::Unknown, "unknown"),
			(OnchainState::UnconfirmedIncoming, "unconfirmed_incoming"),
			(OnchainState::Confirmed, "confirmed"),
			(OnchainState::SpendingInFlight, "spending_in_flight"),
			(OnchainState::Spent, "spent"),
		] {
			assert_eq!(SqliteCodec::encode(value), encoded);
			assert_eq!(SqliteCodec::decode::<OnchainState>(encoded).unwrap(), value);
		}

		for (value, encoded) in [
			(Availability::Unknown, "unknown"),
			(Availability::Selectable, "selectable"),
			(Availability::Reserved, "reserved"),
			(Availability::Locked, "locked"),
			(Availability::BlockedByPolicy, "blocked_by_policy"),
			(Availability::BlockedByL2, "blocked_by_l2"),
			(Availability::Recovering, "recovering"),
		] {
			assert_eq!(SqliteCodec::encode(value), encoded);
			assert_eq!(SqliteCodec::decode::<Availability>(encoded).unwrap(), value);
		}

		for (value, encoded) in [
			(UtxoRole::Unknown, "unknown"),
			(UtxoRole::General, "general"),
			(UtxoRole::SpendPreferred, "spend_preferred"),
			(UtxoRole::Reserve, "reserve"),
			(UtxoRole::RecoveryOutput, "recovery_output"),
			(UtxoRole::ConsolidationCandidate, "consolidation_candidate"),
		] {
			assert_eq!(SqliteCodec::encode(value), encoded);
			assert_eq!(SqliteCodec::decode::<UtxoRole>(encoded).unwrap(), value);
		}
	}

	#[test]
	fn operation_codec_round_trips_current_names() {
		for (value, encoded) in [
			(LockPurpose::ManualReservation, "manual_reservation"),
			(LockPurpose::BtcSend, "btc_send"),
			(LockPurpose::FeeSupport, "fee_support"),
			(LockPurpose::PlannedSelection, "planned_selection"),
		] {
			assert_eq!(SqliteCodec::encode(value), encoded);
			assert_eq!(SqliteCodec::decode::<LockPurpose>(encoded).unwrap(), value);
		}

		for (value, encoded) in [
			(OperationPhase::Selected, "selected"),
			(OperationPhase::Constructing, "constructing"),
			(OperationPhase::Signed, "signed"),
			(OperationPhase::Broadcast, "broadcast"),
			(OperationPhase::AwaitingConfirmation, "awaiting_confirmation"),
			(OperationPhase::AwaitingL2Finality, "awaiting_l2_finality"),
			(OperationPhase::RollingBack, "rolling_back"),
		] {
			assert_eq!(SqliteCodec::encode(value), encoded);
			assert_eq!(SqliteCodec::decode::<OperationPhase>(encoded).unwrap(), value);
		}

		for (value, encoded) in [
			(IntentStatus::Pending, "pending"),
			(IntentStatus::Committed, "committed"),
			(IntentStatus::Failed, "failed"),
		] {
			assert_eq!(SqliteCodec::encode(value), encoded);
			assert_eq!(SqliteCodec::decode::<IntentStatus>(encoded).unwrap(), value);
		}
	}

	#[test]
	fn lock_purpose_codec_preserves_legacy_aliases() {
		for alias in [
			"single_target_reserve",
			"anchor_with_fee_support",
			"rgb_receive_preparation",
			"rgb_l1_send",
			"channel_funding",
		] {
			assert_eq!(
				SqliteCodec::decode::<LockPurpose>(alias).unwrap(),
				LockPurpose::PlannedSelection
			);
		}
	}
}
