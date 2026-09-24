use crate::WalletScope;

pub(crate) fn prefer_scope<'a>(
	scopes: impl Iterator<Item = &'a WalletScope>,
) -> Option<&'a WalletScope> {
	scopes.min_by_key(|s| s.priority)
}
