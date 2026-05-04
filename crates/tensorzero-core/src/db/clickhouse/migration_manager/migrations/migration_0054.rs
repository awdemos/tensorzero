use super::check_column_exists;
use super::check_table_exists;
use crate::db::clickhouse::ClickHouseConnectionInfo;
use crate::db::clickhouse::migration_manager::migration_trait::Migration;
use crate::error::{ErrorDetails, delayed_error::DelayedError};
use async_trait::async_trait;

/// Adds a `canonical_hash` column to `ConfigSnapshot` for ClickHouse parity
/// with the Postgres canonical-hash work (see migration
/// `20260428000000_config_snapshots_jsonb.sql`).
///
/// The legacy `hash` column stays untouched — it still carries the
/// canonical-TOML-bytes hash that every existing `inferences.snapshot_hash`
/// reference expects. The new `canonical_hash` column carries the
/// structural hash from
/// `tensorzero_core::config::snapshot::canonical_hash`, which is what new
/// inference rows reference going forward.
///
/// Existing rows default `canonical_hash` to `0` (UInt256 has no NULL).
/// `0` is the "needs backfill" sentinel — the boot-time backfill in
/// `db::clickhouse::config_queries::backfill_config_snapshot_canonical_hash`
/// re-parses each row's `config` (TOML) text and writes the structural
/// hash. Lookups by canonical hash against rows the backfill hasn't
/// touched (or that fail to backfill) return not-found, which is
/// semantically correct: the row was written before canonical hashing
/// existed and was never identified by it.
pub struct Migration0054<'a> {
    pub clickhouse: &'a ClickHouseConnectionInfo,
}

const MIGRATION_ID: &str = "0054";

#[async_trait]
impl Migration for Migration0054<'_> {
    async fn can_apply(&self) -> Result<(), DelayedError> {
        if !check_table_exists(self.clickhouse, "ConfigSnapshot", MIGRATION_ID).await? {
            return Err(DelayedError::new(ErrorDetails::ClickHouseMigration {
                id: MIGRATION_ID.to_string(),
                message: "`ConfigSnapshot` table does not exist".to_string(),
            }));
        }
        Ok(())
    }

    async fn should_apply(&self) -> Result<bool, DelayedError> {
        let has_canonical_hash = check_column_exists(
            self.clickhouse,
            "ConfigSnapshot",
            "canonical_hash",
            MIGRATION_ID,
        )
        .await?;
        Ok(!has_canonical_hash)
    }

    async fn apply(&self, _clean_start: bool) -> Result<(), DelayedError> {
        let on_cluster_name = self.clickhouse.get_on_cluster_name();

        // `DEFAULT 0` so existing rows have a well-defined value (UInt256
        // has no NULL). `0` doubles as the "needs backfill" sentinel for
        // the boot-time backfill — the canonical hash of any real config
        // is overwhelmingly unlikely to collide with `0` in practice
        // (Blake3-256 collision space).
        self.clickhouse
            .run_query_synchronous_no_params_delayed_err(format!(
                "ALTER TABLE ConfigSnapshot{on_cluster_name} \
                 ADD COLUMN IF NOT EXISTS canonical_hash UInt256 DEFAULT 0"
            ))
            .await?;

        Ok(())
    }

    fn rollback_instructions(&self) -> String {
        format!(
            "/* drop the canonical_hash column added by migration {MIGRATION_ID} */\n\
             ALTER TABLE ConfigSnapshot DROP COLUMN IF EXISTS canonical_hash;"
        )
    }

    async fn has_succeeded(&self) -> Result<bool, DelayedError> {
        // Migration is "succeeded" iff the column now exists. `should_apply`
        // already inverts the same check, so reuse it.
        Ok(!self.should_apply().await?)
    }
}
