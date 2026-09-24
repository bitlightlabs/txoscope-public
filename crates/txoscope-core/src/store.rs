use crate::{
	lock::OperationLock,
	operation::{OperationIntent, OperationPhase, OperationSelection, PersistedOperation},
	reconciliation::ReconcilePlan,
	Account, AccountId, DomainResult, ManagedUtxo, OperationId, OutPoint, WalletScope,
	WalletScopeId,
};

/// Reads state using distinct account, scope, and operation identities.
///
/// ```
/// use txoscope_core::{AccountId, WalletScopeId, OperationId, DomainResult};
/// use txoscope_core::backend::StateRead;
/// fn read(store: &impl StateRead) -> DomainResult<()> {
///     store.get_account(&AccountId::new("account"))?;
///     store.get_scope(&WalletScopeId::new("scope"))?;
///     store.get_operation(&OperationId::new("operation"))?;
///     Ok(())
/// }
/// ```
///
/// A scope identity cannot be used to look up an account.
/// ```compile_fail
/// use txoscope_core::WalletScopeId;
/// use txoscope_core::backend::StateRead;
/// fn wrong(store: &impl StateRead, scope: &WalletScopeId) {
///     store.get_account(scope);
/// }
/// ```
///
/// An operation identity cannot be used to look up a scope.
/// ```compile_fail
/// use txoscope_core::OperationId;
/// use txoscope_core::backend::StateRead;
/// fn wrong(store: &impl StateRead, operation: &OperationId) {
///     store.get_scope(operation);
/// }
/// ```
///
/// An account identity cannot be used to look up an operation.
/// ```compile_fail
/// use txoscope_core::AccountId;
/// use txoscope_core::backend::StateRead;
/// fn wrong(store: &impl StateRead, account: &AccountId) {
///     store.get_operation(account);
/// }
/// ```
pub trait StateRead {
	fn get_account(&self, account_id: &AccountId) -> DomainResult<Option<Account>>;
	fn get_scope(&self, scope_id: &WalletScopeId) -> DomainResult<Option<WalletScope>>;
	fn get_utxo(&self, outpoint: &OutPoint) -> DomainResult<Option<ManagedUtxo>>;
	fn get_lock(&self, outpoint: &OutPoint) -> DomainResult<Option<OperationLock>>;
	fn get_operation(&self, operation_id: &OperationId)
		-> DomainResult<Option<PersistedOperation>>;
	fn get_intent(&self, operation_id: &OperationId) -> DomainResult<Option<OperationIntent>>;
	fn list_utxos(&self) -> DomainResult<Vec<ManagedUtxo>>;
	fn list_operations(&self) -> DomainResult<Vec<PersistedOperation>>;
	fn list_pending_intents(&self) -> DomainResult<Vec<OperationIntent>>;
	fn current_tick(&self) -> DomainResult<u64>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateBatch {
	pub selection: OperationSelection,
	pub phase: OperationPhase,
	pub owner: String,
}

pub trait StateStore: StateRead {
	fn put_account(&mut self, account: Account) -> DomainResult<()>;
	fn put_scope(&mut self, scope: WalletScope) -> DomainResult<()>;
	fn put_utxo(&mut self, utxo: ManagedUtxo) -> DomainResult<()>;
	fn advance_tick(&mut self, delta: u64) -> DomainResult<()>;
	fn persist_intent(&mut self, selection: OperationSelection) -> DomainResult<OperationIntent>;
	fn mark_intent_committed(&mut self, operation_id: &OperationId) -> DomainResult<()>;
	/// Rejects only a pending intent, atomically releasing any applied Selected reservation.
	/// Already failed intents are idempotent; committed intents must not be rejected.
	fn mark_intent_failed(&mut self, operation_id: &OperationId) -> DomainResult<()>;
	/// Validates capabilities and applies all effects atomically against the same state.
	/// Use `OperationSelection::validate_capabilities` inside the write transaction/snapshot.
	fn apply_batch(&mut self, batch: StateBatch) -> DomainResult<()>;
	fn apply_reconciliation(&mut self, plan: ReconcilePlan) -> DomainResult<()>;
	/// Atomically releases only a Selected operation into RollingBack. Returns
	/// None without changing anything if the operation no longer is Selected.
	fn release_selected_operation(
		&mut self, operation_id: &OperationId,
	) -> DomainResult<Option<Vec<OutPoint>>>;
	fn release_operation(
		&mut self, operation_id: &OperationId, released_phase: OperationPhase,
	) -> DomainResult<Vec<OutPoint>>;
	/// Atomically advances the operation and its bound inputs without releasing
	/// locks or bindings. Validate the current transition and committed intent in
	/// the same write transaction. Clear selection expiry on the operation and its locks.
	/// Broadcast and later phases mark inputs SpendingInFlight; earlier phases
	/// preserve their observed state. Returns the outpoints whose bindings advanced.
	fn complete_operation(
		&mut self, operation_id: &OperationId, completed_phase: OperationPhase,
	) -> DomainResult<Vec<OutPoint>>;
}
