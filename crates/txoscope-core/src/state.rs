#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::EnumString, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum OnchainState {
	Unknown,
	UnconfirmedIncoming,
	Confirmed,
	SpendingInFlight,
	Spent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::EnumString, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum Availability {
	Unknown,
	Selectable,
	Reserved,
	Locked,
	BlockedByPolicy,
	BlockedByL2,
	Recovering,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::EnumString, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum UtxoRole {
	Unknown,
	General,
	SpendPreferred,
	Reserve,
	RecoveryOutput,
	ConsolidationCandidate,
}
