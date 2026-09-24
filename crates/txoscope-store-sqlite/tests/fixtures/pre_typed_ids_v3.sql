-- Frozen SQLite v3 schema from txoscope commit
-- 041c2c77bb488b2361cd0fa88b1d8b986a2084f6, before typed identifiers.
-- Keep this fixture independent of current schema/serialization code.
-- Identifier whitespace, Unicode, punctuation, and the opaque txid are intentional.

PRAGMA journal_mode = WAL;
PRAGMA synchronous = FULL;
PRAGMA foreign_keys = ON;

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
    binding_phase TEXT,
    conflict_message TEXT,
    conflict_recorded_at INTEGER
);

CREATE TABLE IF NOT EXISTS locks (
    outpoint TEXT PRIMARY KEY,
    operation_id TEXT NOT NULL,
    account_id TEXT NOT NULL,
    scope_id TEXT NOT NULL,
    purpose TEXT NOT NULL,
    owner TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    expires_at INTEGER
);

CREATE TABLE IF NOT EXISTS operations (
    operation_id TEXT PRIMARY KEY,
    phase TEXT NOT NULL,
    updated_at INTEGER NOT NULL,
    expires_at INTEGER
);

CREATE TABLE IF NOT EXISTS intents (
    operation_id TEXT PRIMARY KEY,
    change_scope_id TEXT NOT NULL,
    status TEXT NOT NULL,
    recorded_at INTEGER NOT NULL,
    expires_at INTEGER
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

INSERT INTO meta(key, value) VALUES('tick', '10');

INSERT INTO accounts(account_id, name, capabilities)
VALUES(' account: Alice / 客户 #01 ', 'Legacy account', 'btc_spend,reserve_support');

INSERT INTO scopes(
    scope_id, account_id, descriptor_ref, derivation_scope, capabilities, priority
)
VALUES(
    ' scope:main/外部 #02 ', ' account: Alice / 客户 #01 ',
    'fixture:descriptor', 'external', 'btc_spend,reserve_support', 7
);

INSERT INTO utxos(
    outpoint, txid, vout, account_id, scope_id, value_sats, onchain_state,
    availability, role, binding_operation_id, binding_phase,
    conflict_message, conflict_recorded_at
)
VALUES(
    'wallet:opaque-tx:4294967295', 'wallet:opaque-tx', 4294967295,
    ' account: Alice / 客户 #01 ', ' scope:main/外部 #02 ', 50000,
    'confirmed', 'reserved', 'general', ' operation:send/签名 #03 ', 'selected',
    NULL, NULL
);

INSERT INTO locks(
    outpoint, operation_id, account_id, scope_id, purpose, owner, created_at, expires_at
)
VALUES(
    'wallet:opaque-tx:4294967295', ' operation:send/签名 #03 ',
    ' account: Alice / 客户 #01 ', ' scope:main/外部 #02 ',
    'planned_selection', 'fixture:owner/旧版', 9, 2000000000
);

INSERT INTO operations(operation_id, phase, updated_at, expires_at)
VALUES(' operation:send/签名 #03 ', 'selected', 9, 2000000000);

INSERT INTO intents(operation_id, change_scope_id, status, recorded_at, expires_at)
VALUES(' operation:send/签名 #03 ', ' scope:main/外部 #02 ', 'committed', 8, 2000000000);

INSERT INTO intent_inputs(operation_id, position, outpoint)
VALUES(' operation:send/签名 #03 ', 0, 'wallet:opaque-tx:4294967295');

INSERT INTO intent_effects(operation_id, position, effect_kind, outpoint, purpose)
VALUES(
    ' operation:send/签名 #03 ', 0, 'lock_utxo',
    'wallet:opaque-tx:4294967295', 'planned_selection'
);

PRAGMA user_version = 3;
