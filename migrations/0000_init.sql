-- Initial schema for the single-PP standin.
--
-- Consolidated: the stack has not been used anywhere, so the whole schema lives
-- in one init migration rather than an incremental chain. Reconstructed from the
-- committed row-shape structs (`crates/persistence/src/models.rs`) and the
-- repository queries (`crates/persistence/src/repo.rs`) — the source of truth.
--
-- Deliberately NO `event_watcher_state` table: the event watcher holds its
-- ledger cursor in memory only (re-syncs all available history + converges by
-- querying the council on boot), so a Postgres wipe fully resets the instance
-- with no surviving state store.
--
-- Foreign keys are intentionally omitted — the repositories use runtime
-- (non-macro) sqlx queries and never rely on referential enforcement; the join
-- fields (e.g. `created_by`) are nullable identity references, not hard FKs.

-- ---- enum types ----
CREATE TYPE entity_status AS ENUM ('UNVERIFIED', 'APPROVED', 'PENDING', 'BLOCKED');
CREATE TYPE account_type AS ENUM ('OPEX', 'USER');
CREATE TYPE session_status AS ENUM ('ACTIVE', 'INACTIVE');
CREATE TYPE challenge_status AS ENUM ('VERIFIED', 'UNVERIFIED');
CREATE TYPE bundle_status AS ENUM ('PENDING', 'PROCESSING', 'COMPLETED', 'EXPIRED', 'FAILED');
CREATE TYPE transaction_status AS ENUM ('UNVERIFIED', 'VERIFIED', 'FAILED');
CREATE TYPE council_membership_status AS ENUM ('PENDING', 'ACTIVE', 'REJECTED');

-- ---- entities ----
-- The entity id IS the wallet public key (single-PP standin collapses the
-- Deno reference's pp_entity_approvals join onto entities directly).
CREATE TABLE entities (
    id            TEXT PRIMARY KEY,
    status        entity_status NOT NULL,
    name          TEXT,
    jurisdictions TEXT[],
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_by    TEXT,
    updated_by    TEXT,
    deleted_at    TIMESTAMPTZ
);

-- ---- accounts ----
CREATE TABLE accounts (
    id         TEXT PRIMARY KEY,
    type       account_type NOT NULL,
    entity_id  TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_by TEXT,
    updated_by TEXT,
    deleted_at TIMESTAMPTZ
);
CREATE INDEX idx_accounts_entity_id ON accounts (entity_id);

-- ---- sessions ----
CREATE TABLE sessions (
    id         TEXT PRIMARY KEY,
    status     session_status NOT NULL,
    jwt_token  TEXT,
    account_id TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_by TEXT,
    updated_by TEXT,
    deleted_at TIMESTAMPTZ
);

-- ---- challenges ----
CREATE TABLE challenges (
    id         TEXT PRIMARY KEY,
    account_id TEXT NOT NULL,
    status     challenge_status NOT NULL,
    ttl        TIMESTAMPTZ NOT NULL,
    tx_hash    TEXT NOT NULL,
    tx_xdr     TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_by TEXT,
    updated_by TEXT,
    deleted_at TIMESTAMPTZ
);
CREATE INDEX idx_challenges_tx_hash ON challenges (tx_hash);

-- ---- operations_bundles ----
CREATE TABLE operations_bundles (
    id                  TEXT PRIMARY KEY,
    status              bundle_status NOT NULL,
    channel_contract_id TEXT,
    ttl                 TIMESTAMPTZ NOT NULL,
    operations_mlxdr    JSONB NOT NULL,
    fee                 BIGINT NOT NULL,
    retry_count         INTEGER NOT NULL DEFAULT 0,
    last_failure_reason TEXT,
    failure_detail      JSONB,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_by          TEXT,
    updated_by          TEXT,
    deleted_at          TIMESTAMPTZ
);
CREATE INDEX idx_operations_bundles_status ON operations_bundles (status);

-- ---- transactions ----
CREATE TABLE transactions (
    id              TEXT PRIMARY KEY,
    status          transaction_status NOT NULL,
    timeout         TIMESTAMPTZ NOT NULL,
    ledger_sequence TEXT NOT NULL,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_by      TEXT,
    updated_by      TEXT,
    deleted_at      TIMESTAMPTZ
);
CREATE INDEX idx_transactions_status ON transactions (status);

-- ---- bundles_transactions (join) ----
CREATE TABLE bundles_transactions (
    bundle_id      TEXT NOT NULL,
    transaction_id TEXT NOT NULL,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_by     TEXT,
    updated_by     TEXT,
    deleted_at     TIMESTAMPTZ,
    PRIMARY KEY (bundle_id, transaction_id)
);

-- ---- utxos ----
CREATE TABLE utxos (
    id                  TEXT PRIMARY KEY,
    amount              BIGINT NOT NULL,
    account_id          TEXT NOT NULL,
    spent_by_account_id TEXT,
    created_at_bundle_id TEXT,
    spent_at_bundle_id  TEXT,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_by          TEXT,
    updated_by          TEXT,
    deleted_at          TIMESTAMPTZ
);
CREATE INDEX idx_utxos_account_unspent ON utxos (account_id) WHERE spent_at_bundle_id IS NULL;

-- ---- wallet_users ----
CREATE TABLE wallet_users (
    public_key TEXT PRIMARY KEY,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- ---- council_memberships ----
-- Single PP, but it can belong to multiple councils; one row per council
-- (council id = channel-auth contract id).
CREATE TABLE council_memberships (
    id                    TEXT PRIMARY KEY,
    council_url           TEXT NOT NULL,
    council_name          TEXT,
    council_public_key    TEXT NOT NULL,
    channel_auth_id       TEXT NOT NULL,
    status                council_membership_status NOT NULL,
    config_json           TEXT,
    claimed_jurisdictions TEXT,
    join_request_id       TEXT,
    created_at            TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at            TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_by            TEXT,
    updated_by            TEXT,
    deleted_at            TIMESTAMPTZ
);
CREATE INDEX idx_council_memberships_channel_auth ON council_memberships (channel_auth_id);

-- ---- mempool_metrics ----
CREATE TABLE mempool_metrics (
    id                 SERIAL PRIMARY KEY,
    recorded_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    platform_version   TEXT NOT NULL,
    queue_depth        INTEGER NOT NULL,
    slot_count         INTEGER NOT NULL,
    bundles_completed  INTEGER NOT NULL,
    bundles_expired    INTEGER NOT NULL,
    bundles_failed     INTEGER NOT NULL,
    avg_processing_ms  DOUBLE PRECISION,
    p95_processing_ms  DOUBLE PRECISION,
    throughput_per_min DOUBLE PRECISION
);
CREATE INDEX idx_mempool_metrics_recorded_at ON mempool_metrics (recorded_at DESC);

-- ---- channel_states (UC6 asset-lifecycle) ----
-- Local view of a council's per-(privacy)channel disable decision. Keyed by the
-- privacy-channel contract id. Driven by channel_state_changed events plus
-- convergence-by-query from the council; the bundle-submit gate consults it to
-- enforce withdraw-only.
CREATE TABLE channel_states (
    channel_contract_id TEXT PRIMARY KEY,
    is_disabled         BOOLEAN NOT NULL DEFAULT FALSE,
    last_event_ledger   BIGINT,
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now()
);
