use crate::{AccountId, CapabilitySet, WalletScopeId};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DescriptorRef(pub String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DerivationScope(pub String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalletScope {
	pub id: WalletScopeId,
	pub account_id: AccountId,
	pub descriptor_ref: DescriptorRef,
	pub derivation_scope: DerivationScope,
	pub capabilities: CapabilitySet,
	pub priority: u32,
}

impl WalletScope {
	pub fn new(
		id: impl Into<WalletScopeId>, account_id: impl Into<AccountId>,
		descriptor_ref: impl Into<String>, derivation_scope: impl Into<String>,
		capabilities: CapabilitySet, priority: u32,
	) -> Self {
		Self {
			id: id.into(),
			account_id: account_id.into(),
			descriptor_ref: DescriptorRef(descriptor_ref.into()),
			derivation_scope: DerivationScope(derivation_scope.into()),
			capabilities,
			priority,
		}
	}
}
