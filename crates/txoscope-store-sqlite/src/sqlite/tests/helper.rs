use tempfile::TempDir;
use txoscope_core::{
	backend::{StateBatch, StateStore},
	Account, Capability, CapabilitySet, ManagedUtxo, OperationId, OperationPhase, OutPoint,
	WalletScope,
};

use super::super::SqliteStateStore;

/// Owns the independent connections and removes their database after they close.
pub(super) struct ReplayFixture {
	pub(super) writer: SqliteStateStore,
	pub(super) replay: SqliteStateStore,
	pub(super) operation_id: OperationId,
	// Field order keeps the directory alive until both connections have closed.
	_directory: TempDir,
}

impl ReplayFixture {
	pub(super) fn new() -> Self {
		let directory = TempDir::new().unwrap();
		let path = directory.path().join("replay.sqlite");
		let mut writer = SqliteStateStore::open(&path).unwrap();
		let capabilities = CapabilitySet::new([Capability::BtcSpend]);
		writer.put_account(Account::new("btc", "BTC", vec![], capabilities.clone())).unwrap();
		writer
			.put_scope(WalletScope::new("scope", "btc", "descriptor", "external", capabilities, 0))
			.unwrap();
		writer
			.put_utxo(ManagedUtxo::new(OutPoint::new("input", 0), "btc", "scope", 60_000))
			.unwrap();

		let selection = super::selection("input");
		let operation_id = selection.operation_id.clone();
		writer.persist_intent(selection.clone()).unwrap();
		writer
			.apply_batch(StateBatch {
				selection,
				phase: OperationPhase::Selected,
				owner: "test".into(),
			})
			.unwrap();
		writer.mark_intent_committed(&operation_id).unwrap();

		let replay = SqliteStateStore::open(path).unwrap();
		Self { writer, replay, operation_id, _directory: directory }
	}
}
