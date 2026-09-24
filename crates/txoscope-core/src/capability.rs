use std::collections::BTreeSet;

/// Account and scope capability flags. New flags may be added; downstream
/// matches must include a wildcard arm for future capabilities.
#[derive(
	Debug,
	Clone,
	Copy,
	PartialEq,
	Eq,
	PartialOrd,
	Ord,
	Hash,
	strum::EnumString,
	strum::IntoStaticStr,
)]
#[strum(serialize_all = "snake_case")]
#[non_exhaustive]
pub enum Capability {
	BtcSpend,
	BtcReceive,
	/// Allows allocation-free inputs to supply bitcoin value in asset selection.
	FeeSupport,
	ReserveSupport,
	#[strum(to_string = "single_target_reserve", serialize = "rgb_receive")]
	SingleTargetReserve,
	L2Settlement,
	ConsolidationSource,
	ConsolidationTarget,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CapabilitySet(BTreeSet<Capability>);

impl CapabilitySet {
	pub fn new(capabilities: impl IntoIterator<Item = Capability>) -> Self {
		Self(capabilities.into_iter().collect())
	}

	pub fn contains(&self, capability: &Capability) -> bool {
		self.0.contains(capability)
	}

	pub fn contains_all(&self, required: &CapabilitySet) -> bool {
		required.0.iter().all(|c| self.contains(c))
	}

	pub fn insert(&mut self, capability: Capability) {
		self.0.insert(capability);
	}

	pub fn iter(&self) -> impl Iterator<Item = &Capability> {
		self.0.iter()
	}
}

impl From<Vec<Capability>> for CapabilitySet {
	fn from(value: Vec<Capability>) -> Self {
		Self::new(value)
	}
}
