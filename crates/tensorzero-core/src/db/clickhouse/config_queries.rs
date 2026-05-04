use std::collections::HashMap;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tensorzero_types::SnapshotHashScheme;

use super::{ClickHouseConnectionInfo, ExternalDataInfo};
use crate::config::snapshot::{ConfigSnapshot, SnapshotHash};
use crate::db::ConfigQueries;
use crate::error::{DelayedError, Error, ErrorDetails};

#[async_trait]
impl ConfigQueries for ClickHouseConnectionInfo {
    /// Looks up a snapshot by hash, dispatching on the hash scheme:
    ///
    /// - `LegacyToml`  → `WHERE hash = $1`           (the canonical-TOML
    ///                                                bytes hash; what
    ///                                                `inferences.snapshot_hash`
    ///                                                rows written before the
    ///                                                canonical-hash migration
    ///                                                reference)
    /// - `Canonical`   → `WHERE canonical_hash = $1` (the structural hash;
    ///                                                what new inference rows
    ///                                                reference going forward)
    ///
    /// Both columns hold `UInt256` values. ClickHouse's `toUInt256(...)`
    /// parser cannot handle the `can:` prefix that `Display` adds to
    /// canonical hashes, so we send the bare decimal via
    /// `to_decimal_string`.
    async fn get_config_snapshot(
        &self,
        snapshot_hash: SnapshotHash,
    ) -> Result<ConfigSnapshot, Error> {
        #[derive(Deserialize)]
        struct ConfigSnapshotRow {
            config: String,
            extra_templates: HashMap<String, String>,
            #[serde(default)]
            tags: HashMap<String, String>,
        }

        let hash_decimal = snapshot_hash.to_decimal_string();
        let hash_column = match snapshot_hash.scheme() {
            SnapshotHashScheme::LegacyToml => "hash",
            SnapshotHashScheme::Canonical => "canonical_hash",
        };
        let query = format!(
            "SELECT config, extra_templates, tags \
             FROM ConfigSnapshot FINAL \
             WHERE {hash_column} = toUInt256('{hash_decimal}') \
             LIMIT 1 \
             FORMAT JSONEachRow"
        );

        let response = self.run_query_synchronous_no_params(query).await?;

        if response.response.is_empty() {
            return Err(Error::new(ErrorDetails::ConfigSnapshotNotFound {
                snapshot_hash: snapshot_hash.to_string(),
            }));
        }

        let row: ConfigSnapshotRow = serde_json::from_str(&response.response).map_err(|e| {
            Error::new(ErrorDetails::ClickHouseDeserialization {
                message: e.to_string(),
            })
        })?;

        ConfigSnapshot::from_stored(&row.config, row.extra_templates, row.tags, &snapshot_hash)
    }

    /// Writes a snapshot to `ConfigSnapshot` populating BOTH `hash`
    /// (legacy canonical-TOML-bytes) and `canonical_hash` (structural).
    /// Either column dispatches the read path correctly via
    /// `get_config_snapshot`.
    async fn write_config_snapshot(&self, snapshot: &ConfigSnapshot) -> Result<(), DelayedError> {
        #[derive(Serialize)]
        struct ConfigSnapshotRow<'a> {
            config: &'a str,
            extra_templates: &'a HashMap<String, String>,
            hash: &'a str,
            canonical_hash: &'a str,
            tensorzero_version: &'static str,
            tags: &'a HashMap<String, String>,
        }

        // The legacy hash basis lives on `snapshot.hash`. Compute the
        // canonical structural hash separately so both columns are
        // populated on every write — that's what makes scheme dispatch
        // in `get_config_snapshot` work for both old and new lookups.
        let legacy_hash = snapshot.hash.clone();
        let canonical_hash = snapshot.config.canonical_hash().map_err(|e| {
            DelayedError::new(ErrorDetails::Serialization {
                message: format!("Failed to compute canonical hash for snapshot write: {e}"),
            })
        })?;
        let legacy_decimal = legacy_hash.to_decimal_string().to_string();
        let canonical_decimal = canonical_hash.to_decimal_string().to_string();

        let config_string = toml::to_string(&snapshot.config).map_err(|e| {
            DelayedError::new(ErrorDetails::Serialization {
                message: format!("Failed to serialize config snapshot: {e}"),
            })
        })?;

        let row = ConfigSnapshotRow {
            config: &config_string,
            extra_templates: &snapshot.extra_templates,
            hash: &legacy_decimal,
            canonical_hash: &canonical_decimal,
            tensorzero_version: crate::endpoints::status::TENSORZERO_VERSION,
            tags: &snapshot.tags,
        };

        let json_data = serde_json::to_string(&row).map_err(|e| {
            DelayedError::new(ErrorDetails::Serialization {
                message: format!("Failed to serialize config snapshot: {e}"),
            })
        })?;

        let external_data = ExternalDataInfo {
            external_data_name: "new_data".to_string(),
            structure: "config String, extra_templates Map(String, String), hash String, canonical_hash String, tensorzero_version String, tags Map(String, String)".to_string(),
            format: "JSONEachRow".to_string(),
            data: json_data,
        };

        let query = format!(
            r"INSERT INTO ConfigSnapshot
(config, extra_templates, hash, canonical_hash, tensorzero_version, tags, created_at, last_used)
SELECT
    new_data.config,
    new_data.extra_templates,
    toUInt256(new_data.hash) as hash,
    toUInt256(new_data.canonical_hash) as canonical_hash,
    new_data.tensorzero_version,
    mapUpdate(
        (SELECT any(tags) FROM ConfigSnapshot FINAL WHERE hash = toUInt256('{legacy_decimal}')),
        new_data.tags
    ) as tags,
    ifNull((SELECT any(created_at) FROM ConfigSnapshot FINAL WHERE hash = toUInt256('{legacy_decimal}')), now64()) as created_at,
    now64() as last_used
FROM new_data"
        );

        self.run_query_with_external_data(external_data, query)
            .await?;

        Ok(())
    }
}

/// Best-effort backfill of the `canonical_hash` column on `ConfigSnapshot`
/// rows that predate it.
///
/// New rows are written with both `hash` and `canonical_hash` populated;
/// rows that existed before migration `0054` ran have `canonical_hash = 0`
/// (the `DEFAULT` we picked because UInt256 has no NULL). This function
/// scans for those rows, re-parses each row's `config` (TOML) text into
/// a `StoredConfig`, computes the structural canonical hash, and
/// re-inserts the row with the new column populated. ClickHouse's
/// `ReplacingMergeTree` engine deduplicates by the `(hash)` ORDER BY
/// key on background merges; reads use `FINAL` so the new version is
/// observed immediately.
///
/// Per-row failures are logged at `error!` level (not `warn!`) and
/// skipped — startup continues even if every row fails. A skipped row
/// remains readable via the legacy `hash` lookup; only canonical-hash
/// lookups against it return not-found, which is the same behavior as
/// if the backfill had not run at all.
///
/// Idempotent: filters by `canonical_hash = 0`, so a row that was
/// already backfilled is never touched again. Cooperative — no locks
/// held; concurrent writers are safe.
pub async fn backfill_config_snapshot_canonical_hash(
    clickhouse: &ClickHouseConnectionInfo,
) -> Result<(), Error> {
    use crate::config::snapshot::StoredConfig;

    #[derive(Deserialize)]
    struct LegacyRow {
        hash: String,
        config: String,
    }

    // Sentinel: rows with `canonical_hash = 0` were written before
    // migration 0054 added the column (or by a write that failed to
    // populate it — also worth surfacing). The Blake3 collision space
    // makes a real config canonicalizing to literal `0` astronomically
    // unlikely.
    let scan_query = "SELECT toString(hash) AS hash, config \
                      FROM ConfigSnapshot FINAL \
                      WHERE canonical_hash = 0 \
                      FORMAT JSONEachRow"
        .to_string();
    let response = clickhouse
        .run_query_synchronous_no_params(scan_query)
        .await?;

    if response.response.is_empty() {
        return Ok(());
    }

    let mut total_backfilled: u64 = 0;
    let mut total_skipped: u64 = 0;

    for line in response.response.lines() {
        if line.is_empty() {
            continue;
        }
        let row: LegacyRow = serde_json::from_str(line).map_err(|e| {
            Error::new(ErrorDetails::ClickHouseDeserialization {
                message: format!("config_snapshots backfill: malformed JSONEachRow line: {e}"),
            })
        })?;

        let stored: StoredConfig = match toml::from_str(&row.config) {
            Ok(v) => v,
            Err(e) => {
                tracing::error!(
                    hash = %row.hash,
                    "ConfigSnapshot backfill: skipping unparseable TOML \
                     (file a bug if you don't expect this — backfill is best-effort, \
                     the row stays readable via legacy hash lookup): {e}"
                );
                total_skipped += 1;
                continue;
            }
        };
        let canonical_hash = match stored.canonical_hash() {
            Ok(h) => h,
            Err(e) => {
                tracing::error!(
                    hash = %row.hash,
                    "ConfigSnapshot backfill: skipping (canonical_hash computation failed \
                     — the algorithm should be infallible for any valid StoredConfig; \
                     please file a bug): {e}"
                );
                total_skipped += 1;
                continue;
            }
        };
        let canonical_decimal = canonical_hash.to_decimal_string();

        // Re-INSERT the row with `canonical_hash` populated. The
        // ReplacingMergeTree deduplicates by `(hash)` ORDER BY key on
        // background merges; reads using `FINAL` see the new version
        // immediately. We preserve `created_at` from the original row
        // and refresh `last_used` to now64() — same convention as
        // `write_config_snapshot`.
        let legacy_decimal = &row.hash;
        let update_query = format!(
            r"INSERT INTO ConfigSnapshot
(config, extra_templates, hash, canonical_hash, tensorzero_version, tags, created_at, last_used)
SELECT
    config,
    extra_templates,
    hash,
    toUInt256('{canonical_decimal}') as canonical_hash,
    tensorzero_version,
    tags,
    created_at,
    now64() as last_used
FROM ConfigSnapshot FINAL
WHERE hash = toUInt256('{legacy_decimal}')"
        );
        if let Err(e) = clickhouse
            .run_query_synchronous_no_params(update_query)
            .await
        {
            tracing::error!(
                hash = %row.hash,
                "ConfigSnapshot backfill: re-INSERT failed (please file a bug): {e}"
            );
            total_skipped += 1;
            continue;
        }
        total_backfilled += 1;
    }

    if total_backfilled > 0 || total_skipped > 0 {
        tracing::info!(
            backfilled = total_backfilled,
            skipped = total_skipped,
            "ConfigSnapshot canonical_hash backfill complete",
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::db::clickhouse::clickhouse_client::MockClickHouseClient;
    use crate::db::clickhouse::{ClickHouseResponse, ClickHouseResponseMetadata};
    use crate::db::test_helpers::assert_query_contains;

    #[tokio::test]
    async fn test_get_config_snapshot_found() {
        let mut mock = MockClickHouseClient::new();

        mock.expect_run_query_synchronous()
            .withf(|query, _params| {
                assert_query_contains(query, "SELECT config, extra_templates, tags");
                assert_query_contains(query, "FROM ConfigSnapshot FINAL");
                assert_query_contains(query, "LIMIT 1");
                assert_query_contains(query, "FORMAT JSONEachRow");
                true
            })
            .returning(|_, _| {
                let response =
                    r#"{"config":"[functions]\n","extra_templates":{},"tags":{"env":"test"}}"#;
                Ok(ClickHouseResponse {
                    response: response.to_string(),
                    metadata: ClickHouseResponseMetadata {
                        read_rows: 1,
                        written_rows: 0,
                    },
                })
            });

        let conn = ClickHouseConnectionInfo::new_mock(Arc::new(mock));
        let hash = SnapshotHash::new_test();
        let result = conn.get_config_snapshot(hash).await;
        assert!(result.is_ok(), "Should successfully parse config snapshot");
    }

    #[tokio::test]
    async fn test_get_config_snapshot_not_found() {
        let mut mock = MockClickHouseClient::new();

        mock.expect_run_query_synchronous().returning(|_, _| {
            Ok(ClickHouseResponse {
                response: String::new(),
                metadata: ClickHouseResponseMetadata {
                    read_rows: 0,
                    written_rows: 0,
                },
            })
        });

        let conn = ClickHouseConnectionInfo::new_mock(Arc::new(mock));
        let hash = SnapshotHash::new_test();
        let result = conn.get_config_snapshot(hash).await;
        assert!(
            result.is_err(),
            "Should return error when snapshot not found"
        );
        let err = result.unwrap_err();
        assert!(
            matches!(
                err.get_details(),
                ErrorDetails::ConfigSnapshotNotFound { .. }
            ),
            "Error should be ConfigSnapshotNotFound"
        );
    }

    #[tokio::test]
    #[expect(clippy::disallowed_methods)]
    async fn test_write_config_snapshot() {
        let mut mock = MockClickHouseClient::new();

        mock.expect_run_query_with_external_data()
            .withf(|external_data, query| {
                assert_query_contains(query, "INSERT INTO ConfigSnapshot");
                assert_eq!(
                    external_data.external_data_name, "new_data",
                    "External data name should be `new_data`"
                );
                assert_eq!(
                    external_data.format, "JSONEachRow",
                    "Format should be JSONEachRow"
                );
                assert!(
                    external_data.structure.contains("config String"),
                    "Structure should include config column"
                );
                true
            })
            .returning(|_, _| {
                Ok(ClickHouseResponse {
                    response: String::new(),
                    metadata: ClickHouseResponseMetadata {
                        read_rows: 0,
                        written_rows: 1,
                    },
                })
            });

        let conn = ClickHouseConnectionInfo::new_mock(Arc::new(mock));
        let snapshot = ConfigSnapshot::new_empty_for_test();
        let result = conn.write_config_snapshot(&snapshot).await;
        assert!(result.is_ok(), "Should successfully write config snapshot");
    }
}
