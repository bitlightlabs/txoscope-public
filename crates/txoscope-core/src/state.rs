#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::EnumString, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
#[cfg_attr(test, derive(strum::EnumIter))]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "schema", schema(rename_all = "snake_case"))]
pub enum OnchainState {
	Unknown,
	UnconfirmedIncoming,
	Confirmed,
	SpendingInFlight,
	Spent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::EnumString, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
#[cfg_attr(test, derive(strum::EnumIter))]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "schema", schema(rename_all = "snake_case"))]
pub enum Availability {
	Unknown,
	Selectable,
	Reserved,
	Locked,
	BlockedByPolicy,
	/// The output is spent or is being spent by a broadcast transaction.
	BlockedBySpend,
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
