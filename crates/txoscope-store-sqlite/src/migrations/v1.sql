CREATE TABLE IF NOT EXISTS meta (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS accounts (
    account_id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    capabilities TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS scopes (
    scope_id TEXT PRIMARY KEY,
    account_id TEXT NOT NULL,
    descriptor_ref TEXT NOT NULL,
    derivation_scope TEXT NOT NULL,
    capabilities TEXT NOT NULL,
    priority INTEGER NOT NULL,
    FOREIGN KEY(account_id) REFERENCES accounts(account_id)
);

CREATE TABLE IF NOT EXISTS utxos (
    outpoint TEXT PRIMARY KEY,
    txid TEXT NOT NULL,
    vout INTEGER NOT NULL,
    account_id TEXT NOT NULL,
    scope_id TEXT NOT NULL,
    value_sats INTEGER NOT NULL,
    onchain_state TEXT NOT NULL,
    availability TEXT NOT NULL,
    role TEXT NOT NULL,
    binding_operation_id TEXT,
    binding_phase TEXT
);

CREATE TABLE IF NOT EXISTS locks (
    outpoint TEXT PRIMARY KEY,
    operation_id TEXT NOT NULL,
    account_id TEXT NOT NULL,
    scope_id TEXT NOT NULL,
    purpose TEXT NOT NULL,
    owner TEXT NOT NULL,
    created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS operations (
    operation_id TEXT PRIMARY KEY,
    phase TEXT NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS intents (
    operation_id TEXT PRIMARY KEY,
    change_scope_id TEXT NOT NULL,
    status TEXT NOT NULL,
    recorded_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS intent_inputs (
    operation_id TEXT NOT NULL,
    position INTEGER NOT NULL,
    outpoint TEXT NOT NULL,
    PRIMARY KEY(operation_id, position),
    FOREIGN KEY(operation_id) REFERENCES intents(operation_id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS intent_effects (
    operation_id TEXT NOT NULL,
    position INTEGER NOT NULL,
    effect_kind TEXT NOT NULL,
    outpoint TEXT NOT NULL,
    purpose TEXT NOT NULL,
    PRIMARY KEY(operation_id, position),
    FOREIGN KEY(operation_id) REFERENCES intents(operation_id) ON DELETE CASCADE
);
