use crate::{AccountId, CapabilitySet, WalletScopeId};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
	pub id: AccountId,
	pub name: String,
	pub scopes: Vec<WalletScopeId>,
	pub capabilities: CapabilitySet,
}

impl Account {
	pub fn new(
		id: impl Into<AccountId>, name: impl Into<String>, scopes: Vec<WalletScopeId>,
		capabilities: CapabilitySet,
	) -> Self {
		Self { id: id.into(), name: name.into(), scopes, capabilities }
	}
}
