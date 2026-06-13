//! Repositories: one struct per table, each holding a `PgPool` and exposing CRUD methods.
//!
//! Patterns:
//! - Runtime SQL via `sqlx::query` / `sqlx::query_as` (no compile-time verification — avoids
//!   needing a live DATABASE_URL at build time).
//! - Soft-delete aware: read queries filter `deleted_at IS NULL`.

use crate::models::*;
use crate::db::PgPool;
use anyhow::Result;
use chrono::{DateTime, Utc};
use serde_json::Value as JsonValue;
use sqlx::Row;

// ---- entities ----

pub struct EntityRepo {
    pool: PgPool,
}

impl EntityRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn create(
        &self,
        id: &str,
        status: EntityStatus,
        name: Option<&str>,
        jurisdictions: Option<&[String]>,
        created_by: Option<&str>,
    ) -> Result<Entity> {
        let row = sqlx::query_as::<_, Entity>(
            r#"INSERT INTO entities (id, status, name, jurisdictions, created_by, updated_by)
               VALUES ($1, $2, $3, $4, $5, $5)
               RETURNING id, status, name, jurisdictions, created_at, updated_at, created_by, updated_by, deleted_at"#,
        )
        .bind(id)
        .bind(status)
        .bind(name)
        .bind(jurisdictions)
        .bind(created_by)
        .fetch_one(&self.pool)
        .await?;
        Ok(row)
    }

    pub async fn find_by_id(&self, id: &str) -> Result<Option<Entity>> {
        let row = sqlx::query_as::<_, Entity>(
            r#"SELECT id, status, name, jurisdictions, created_at, updated_at, created_by, updated_by, deleted_at
               FROM entities WHERE id = $1 AND deleted_at IS NULL"#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    pub async fn set_status(&self, id: &str, status: EntityStatus) -> Result<()> {
        sqlx::query(r#"UPDATE entities SET status = $2, updated_at = now() WHERE id = $1"#)
            .bind(id)
            .bind(status)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Upgrade an existing entity row (typically created by `record_interaction`
    /// with status=UNVERIFIED) to APPROVED while also populating the identity
    /// fields the operator-facing entities view surfaces. Mirrors the Deno
    /// reference's full upsert path in
    /// `provider-platform/src/core/service/auth/challenge/store/attach-entity-status.ts`.
    pub async fn approve_with_identity(
        &self,
        id: &str,
        name: Option<&str>,
        jurisdictions: Option<&[String]>,
    ) -> Result<()> {
        sqlx::query(
            r#"UPDATE entities
                  SET status = 'APPROVED'::entity_status,
                      name = COALESCE($2, name),
                      jurisdictions = COALESCE($3, jurisdictions),
                      updated_at = now()
                WHERE id = $1"#,
        )
        .bind(id)
        .bind(name)
        .bind(jurisdictions)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Record that `pubkey` interacted with this PP (single-PP standin maps the
    /// Deno reference's per-PP approval table onto the `entities` table itself).
    /// Locked write-invariant — mirrors
    /// `provider-platform/src/persistence/drizzle/repository/pp-entity-approval.repository.ts::recordInteraction`:
    ///   - no row              → insert `UNVERIFIED`
    ///   - row is `UNVERIFIED` → bump `updated_at` only
    ///   - row APPROVED/PENDING/BLOCKED → no-op (status + timestamps untouched)
    pub async fn record_interaction(&self, pubkey: &str) -> Result<()> {
        sqlx::query(
            r#"INSERT INTO entities (id, status)
               VALUES ($1, 'UNVERIFIED'::entity_status)
               ON CONFLICT (id) DO UPDATE
                 SET updated_at = now()
                 WHERE entities.status = 'UNVERIFIED'::entity_status
                   AND entities.deleted_at IS NULL"#,
        )
        .bind(pubkey)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// All non-deleted entity rows, newest interaction first. Mirrors
    /// `pp-entity-approval.repository.ts::listByPp` — the single-PP standin
    /// collapses the join across `pp_entity_approvals → account → entity`
    /// into a direct read of `entities` since the entity id IS the pubkey.
    pub async fn list_all_by_updated(&self) -> Result<Vec<Entity>> {
        let rows = sqlx::query_as::<_, Entity>(
            r#"SELECT id, status, name, jurisdictions, created_at, updated_at, created_by, updated_by, deleted_at
               FROM entities
               WHERE deleted_at IS NULL
               ORDER BY updated_at DESC"#,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }
}

// ---- accounts ----

pub struct AccountRepo {
    pool: PgPool,
}

impl AccountRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn create(
        &self,
        id: &str,
        account_type: AccountType,
        entity_id: &str,
        created_by: Option<&str>,
    ) -> Result<Account> {
        let row = sqlx::query_as::<_, Account>(
            r#"INSERT INTO accounts (id, type, entity_id, created_by, updated_by)
               VALUES ($1, $2, $3, $4, $4)
               RETURNING id, type, entity_id, created_at, updated_at, created_by, updated_by, deleted_at"#,
        )
        .bind(id)
        .bind(account_type)
        .bind(entity_id)
        .bind(created_by)
        .fetch_one(&self.pool)
        .await?;
        Ok(row)
    }

    pub async fn find_by_id(&self, id: &str) -> Result<Option<Account>> {
        let row = sqlx::query_as::<_, Account>(
            r#"SELECT id, type, entity_id, created_at, updated_at, created_by, updated_by, deleted_at
               FROM accounts WHERE id = $1 AND deleted_at IS NULL"#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    pub async fn list_by_entity(&self, entity_id: &str) -> Result<Vec<Account>> {
        let rows = sqlx::query_as::<_, Account>(
            r#"SELECT id, type, entity_id, created_at, updated_at, created_by, updated_by, deleted_at
               FROM accounts WHERE entity_id = $1 AND deleted_at IS NULL ORDER BY created_at"#,
        )
        .bind(entity_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }
}

// ---- sessions ----

pub struct SessionRepo {
    pool: PgPool,
}

impl SessionRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn create(
        &self,
        id: &str,
        account_id: &str,
        jwt_token: Option<&str>,
    ) -> Result<Session> {
        let row = sqlx::query_as::<_, Session>(
            r#"INSERT INTO sessions (id, status, jwt_token, account_id)
               VALUES ($1, 'ACTIVE'::session_status, $2, $3)
               RETURNING id, status, jwt_token, account_id, created_at, updated_at, created_by, updated_by, deleted_at"#,
        )
        .bind(id)
        .bind(jwt_token)
        .bind(account_id)
        .fetch_one(&self.pool)
        .await?;
        Ok(row)
    }

    pub async fn find_by_id(&self, id: &str) -> Result<Option<Session>> {
        let row = sqlx::query_as::<_, Session>(
            r#"SELECT id, status, jwt_token, account_id, created_at, updated_at, created_by, updated_by, deleted_at
               FROM sessions WHERE id = $1 AND deleted_at IS NULL"#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    pub async fn deactivate(&self, id: &str) -> Result<()> {
        sqlx::query(r#"UPDATE sessions SET status = 'INACTIVE'::session_status, updated_at = now() WHERE id = $1"#)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

// ---- challenges ----

pub struct ChallengeRepo {
    pool: PgPool,
}

impl ChallengeRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn create(
        &self,
        id: &str,
        account_id: &str,
        ttl: DateTime<Utc>,
        tx_hash: &str,
        tx_xdr: &str,
    ) -> Result<Challenge> {
        let row = sqlx::query_as::<_, Challenge>(
            r#"INSERT INTO challenges (id, account_id, status, ttl, tx_hash, tx_xdr)
               VALUES ($1, $2, 'UNVERIFIED'::challenge_status, $3, $4, $5)
               RETURNING id, account_id, status, ttl, tx_hash, tx_xdr, created_at, updated_at, created_by, updated_by, deleted_at"#,
        )
        .bind(id)
        .bind(account_id)
        .bind(ttl)
        .bind(tx_hash)
        .bind(tx_xdr)
        .fetch_one(&self.pool)
        .await?;
        Ok(row)
    }

    pub async fn find_by_tx_hash(&self, tx_hash: &str) -> Result<Option<Challenge>> {
        let row = sqlx::query_as::<_, Challenge>(
            r#"SELECT id, account_id, status, ttl, tx_hash, tx_xdr, created_at, updated_at, created_by, updated_by, deleted_at
               FROM challenges WHERE tx_hash = $1 AND deleted_at IS NULL"#,
        )
        .bind(tx_hash)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    pub async fn mark_verified(&self, id: &str) -> Result<()> {
        sqlx::query(r#"UPDATE challenges SET status = 'VERIFIED'::challenge_status, updated_at = now() WHERE id = $1"#)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

// ---- operations_bundles ----

pub struct OperationsBundleRepo {
    pool: PgPool,
}

impl OperationsBundleRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn create(
        &self,
        id: &str,
        ttl: DateTime<Utc>,
        operations_mlxdr: &JsonValue,
        fee: i64,
        channel_contract_id: Option<&str>,
        created_by: Option<&str>,
    ) -> Result<OperationsBundle> {
        let row = sqlx::query_as::<_, OperationsBundle>(
            r#"INSERT INTO operations_bundles (id, status, ttl, operations_mlxdr, fee, channel_contract_id, created_by, updated_by)
               VALUES ($1, 'PENDING'::bundle_status, $2, $3, $4, $5, $6, $6)
               RETURNING id, status, channel_contract_id, ttl, operations_mlxdr, fee, retry_count, last_failure_reason, failure_detail, created_at, updated_at, created_by, updated_by, deleted_at"#,
        )
        .bind(id)
        .bind(ttl)
        .bind(operations_mlxdr)
        .bind(fee)
        .bind(channel_contract_id)
        .bind(created_by)
        .fetch_one(&self.pool)
        .await?;
        Ok(row)
    }

    pub async fn find_by_id(&self, id: &str) -> Result<Option<OperationsBundle>> {
        let row = sqlx::query_as::<_, OperationsBundle>(
            r#"SELECT id, status, channel_contract_id, ttl, operations_mlxdr, fee, retry_count, last_failure_reason, failure_detail, created_at, updated_at, created_by, updated_by, deleted_at
               FROM operations_bundles WHERE id = $1 AND deleted_at IS NULL"#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    pub async fn list_by_status(&self, status: BundleStatus, limit: i64) -> Result<Vec<OperationsBundle>> {
        let rows = sqlx::query_as::<_, OperationsBundle>(
            r#"SELECT id, status, channel_contract_id, ttl, operations_mlxdr, fee, retry_count, last_failure_reason, failure_detail, created_at, updated_at, created_by, updated_by, deleted_at
               FROM operations_bundles WHERE status = $1 AND deleted_at IS NULL ORDER BY created_at DESC LIMIT $2"#,
        )
        .bind(status)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    pub async fn set_status(&self, id: &str, status: BundleStatus) -> Result<()> {
        sqlx::query(r#"UPDATE operations_bundles SET status = $2, updated_at = now() WHERE id = $1"#)
            .bind(id)
            .bind(status)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn mark_failed(&self, id: &str, reason: &str, detail: Option<&JsonValue>) -> Result<()> {
        sqlx::query(
            r#"UPDATE operations_bundles
               SET status = 'FAILED'::bundle_status, last_failure_reason = $2, failure_detail = $3,
                   retry_count = retry_count + 1, updated_at = now()
               WHERE id = $1"#,
        )
        .bind(id)
        .bind(reason)
        .bind(detail)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

// ---- transactions ----

pub struct TransactionRepo {
    pool: PgPool,
}

impl TransactionRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn create(
        &self,
        id: &str,
        timeout: DateTime<Utc>,
        ledger_sequence: &str,
    ) -> Result<Transaction> {
        let row = sqlx::query_as::<_, Transaction>(
            r#"INSERT INTO transactions (id, status, timeout, ledger_sequence)
               VALUES ($1, 'UNVERIFIED'::transaction_status, $2, $3)
               RETURNING id, status, timeout, ledger_sequence, created_at, updated_at, created_by, updated_by, deleted_at"#,
        )
        .bind(id)
        .bind(timeout)
        .bind(ledger_sequence)
        .fetch_one(&self.pool)
        .await?;
        Ok(row)
    }

    pub async fn find_by_id(&self, id: &str) -> Result<Option<Transaction>> {
        let row = sqlx::query_as::<_, Transaction>(
            r#"SELECT id, status, timeout, ledger_sequence, created_at, updated_at, created_by, updated_by, deleted_at
               FROM transactions WHERE id = $1 AND deleted_at IS NULL"#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    pub async fn list_unverified(&self, limit: i64) -> Result<Vec<Transaction>> {
        let rows = sqlx::query_as::<_, Transaction>(
            r#"SELECT id, status, timeout, ledger_sequence, created_at, updated_at, created_by, updated_by, deleted_at
               FROM transactions WHERE status = 'UNVERIFIED'::transaction_status AND deleted_at IS NULL ORDER BY created_at LIMIT $1"#,
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    pub async fn set_status(&self, id: &str, status: TransactionStatus) -> Result<()> {
        sqlx::query(r#"UPDATE transactions SET status = $2, updated_at = now() WHERE id = $1"#)
            .bind(id)
            .bind(status)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

// ---- bundles_transactions ----

pub struct BundleTransactionRepo {
    pool: PgPool,
}

impl BundleTransactionRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn link(&self, bundle_id: &str, transaction_id: &str) -> Result<()> {
        sqlx::query(
            r#"INSERT INTO bundles_transactions (bundle_id, transaction_id)
               VALUES ($1, $2) ON CONFLICT DO NOTHING"#,
        )
        .bind(bundle_id)
        .bind(transaction_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn list_transactions_for_bundle(&self, bundle_id: &str) -> Result<Vec<String>> {
        let rows = sqlx::query(r#"SELECT transaction_id FROM bundles_transactions WHERE bundle_id = $1"#)
            .bind(bundle_id)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.into_iter().map(|r| r.get::<String, _>(0)).collect())
    }

    pub async fn list_bundles_for_transaction(&self, transaction_id: &str) -> Result<Vec<String>> {
        let rows = sqlx::query(r#"SELECT bundle_id FROM bundles_transactions WHERE transaction_id = $1"#)
            .bind(transaction_id)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.into_iter().map(|r| r.get::<String, _>(0)).collect())
    }
}

// ---- utxos ----

pub struct UtxoRepo {
    pool: PgPool,
}

impl UtxoRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn create(
        &self,
        id: &str,
        amount: i64,
        account_id: &str,
        created_at_bundle_id: Option<&str>,
        created_by: Option<&str>,
    ) -> Result<Utxo> {
        let row = sqlx::query_as::<_, Utxo>(
            r#"INSERT INTO utxos (id, amount, account_id, created_at_bundle_id, created_by, updated_by)
               VALUES ($1, $2, $3, $4, $5, $5)
               RETURNING id, amount, account_id, spent_by_account_id, created_at_bundle_id, spent_at_bundle_id, created_at, updated_at, created_by, updated_by, deleted_at"#,
        )
        .bind(id)
        .bind(amount)
        .bind(account_id)
        .bind(created_at_bundle_id)
        .bind(created_by)
        .fetch_one(&self.pool)
        .await?;
        Ok(row)
    }

    pub async fn mark_spent(
        &self,
        id: &str,
        spent_by_account_id: &str,
        spent_at_bundle_id: &str,
    ) -> Result<()> {
        sqlx::query(
            r#"UPDATE utxos
               SET spent_by_account_id = $2, spent_at_bundle_id = $3, updated_at = now()
               WHERE id = $1"#,
        )
        .bind(id)
        .bind(spent_by_account_id)
        .bind(spent_at_bundle_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn list_unspent_for_account(&self, account_id: &str) -> Result<Vec<Utxo>> {
        let rows = sqlx::query_as::<_, Utxo>(
            r#"SELECT id, amount, account_id, spent_by_account_id, created_at_bundle_id, spent_at_bundle_id, created_at, updated_at, created_by, updated_by, deleted_at
               FROM utxos
               WHERE account_id = $1 AND spent_at_bundle_id IS NULL AND deleted_at IS NULL
               ORDER BY created_at"#,
        )
        .bind(account_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }
}

// ---- wallet_users ----

pub struct WalletUserRepo {
    pool: PgPool,
}

impl WalletUserRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn find_or_create(&self, public_key: &str) -> Result<WalletUser> {
        let row = sqlx::query_as::<_, WalletUser>(
            r#"INSERT INTO wallet_users (public_key) VALUES ($1)
               ON CONFLICT (public_key) DO UPDATE SET public_key = EXCLUDED.public_key
               RETURNING public_key, created_at"#,
        )
        .bind(public_key)
        .fetch_one(&self.pool)
        .await?;
        Ok(row)
    }
}

// ---- council_memberships ----

pub struct CouncilMembershipRepo {
    pool: PgPool,
}

impl CouncilMembershipRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn create(
        &self,
        id: &str,
        council_url: &str,
        council_public_key: &str,
        channel_auth_id: &str,
        claimed_jurisdictions: Option<&str>,
    ) -> Result<CouncilMembership> {
        let row = sqlx::query_as::<_, CouncilMembership>(
            r#"INSERT INTO council_memberships
               (id, council_url, council_public_key, channel_auth_id, status, claimed_jurisdictions)
               VALUES ($1, $2, $3, $4, 'PENDING'::council_membership_status, $5)
               RETURNING id, council_url, council_name, council_public_key, channel_auth_id, status,
                         config_json, claimed_jurisdictions, join_request_id,
                         created_at, updated_at, created_by, updated_by, deleted_at"#,
        )
        .bind(id)
        .bind(council_url)
        .bind(council_public_key)
        .bind(channel_auth_id)
        .bind(claimed_jurisdictions)
        .fetch_one(&self.pool)
        .await?;
        Ok(row)
    }

    pub async fn set_status(
        &self,
        channel_auth_id: &str,
        status: CouncilMembershipStatus,
    ) -> Result<()> {
        sqlx::query(
            r#"UPDATE council_memberships SET status = $2, updated_at = now()
               WHERE channel_auth_id = $1"#,
        )
        .bind(channel_auth_id)
        .bind(status)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn find_by_channel_auth(&self, channel_auth_id: &str) -> Result<Option<CouncilMembership>> {
        let row = sqlx::query_as::<_, CouncilMembership>(
            r#"SELECT id, council_url, council_name, council_public_key, channel_auth_id, status,
                      config_json, claimed_jurisdictions, join_request_id,
                      created_at, updated_at, created_by, updated_by, deleted_at
               FROM council_memberships
               WHERE channel_auth_id = $1 AND deleted_at IS NULL"#,
        )
        .bind(channel_auth_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    pub async fn list_active(&self) -> Result<Vec<CouncilMembership>> {
        let rows = sqlx::query_as::<_, CouncilMembership>(
            r#"SELECT id, council_url, council_name, council_public_key, channel_auth_id, status,
                      config_json, claimed_jurisdictions, join_request_id,
                      created_at, updated_at, created_by, updated_by, deleted_at
               FROM council_memberships
               WHERE deleted_at IS NULL
               ORDER BY created_at DESC"#,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }
}

// ---- mempool_metrics ----

pub struct MempoolMetricRepo {
    pool: PgPool,
}

impl MempoolMetricRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn insert_snapshot(
        &self,
        platform_version: &str,
        queue_depth: i32,
        slot_count: i32,
        bundles_completed: i32,
        bundles_expired: i32,
        bundles_failed: i32,
        avg_processing_ms: Option<f64>,
        p95_processing_ms: Option<f64>,
        throughput_per_min: Option<f64>,
    ) -> Result<()> {
        sqlx::query(
            r#"INSERT INTO mempool_metrics
               (platform_version, queue_depth, slot_count, bundles_completed, bundles_expired, bundles_failed,
                avg_processing_ms, p95_processing_ms, throughput_per_min)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)"#,
        )
        .bind(platform_version)
        .bind(queue_depth)
        .bind(slot_count)
        .bind(bundles_completed)
        .bind(bundles_expired)
        .bind(bundles_failed)
        .bind(avg_processing_ms)
        .bind(p95_processing_ms)
        .bind(throughput_per_min)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn list_recent(&self, limit: i64) -> Result<Vec<MempoolMetric>> {
        let rows = sqlx::query_as::<_, MempoolMetric>(
            r#"SELECT id, recorded_at, platform_version, queue_depth, slot_count,
                      bundles_completed, bundles_expired, bundles_failed,
                      avg_processing_ms, p95_processing_ms, throughput_per_min
               FROM mempool_metrics ORDER BY recorded_at DESC LIMIT $1"#,
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }
}

// ---- event_watcher_state ----

pub struct EventWatcherStateRepo {
    pool: PgPool,
}

impl EventWatcherStateRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn get(&self, key: &str) -> Result<Option<String>> {
        let row: Option<(String,)> = sqlx::query_as(
            r#"SELECT value FROM event_watcher_state WHERE key = $1"#,
        )
        .bind(key)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|(v,)| v))
    }

    pub async fn set(&self, key: &str, value: &str) -> Result<()> {
        sqlx::query(
            r#"INSERT INTO event_watcher_state (key, value)
               VALUES ($1, $2)
               ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value, updated_at = now()"#,
        )
        .bind(key)
        .bind(value)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}
