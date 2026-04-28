//! REST bootstrap fixture loader.
//!
//! This module contains a single `#[ignore]`d test (`rest_bootstrap_fixture_config`)
//! whose job is to act as a one-shot "install the fixture config into an empty
//! gateway via the REST apply endpoint" utility for CI.
//!
//! The test is hidden behind `#[ignore]` on purpose: in the regular e2e run
//! it would be wasted work (the gateway is already seeded via
//! `gateway --migrate-config` or the `fixtures` service). In the new
//! `rest-bootstrap` CI job the test is invoked explicitly with
//! `nextest run --run-ignored only -E 'test(rest_bootstrap::fixture_config)'`
//! as a pre-suite step, against a gateway booted empty with
//! `ENABLE_CONFIG_IN_DATABASE=true`.
//!
//! The fixture source is controlled by `TENSORZERO_REST_BOOTSTRAP_CONFIG_PATH`
//! (a glob passed the same way `--config-file` expects), defaulting to the
//! `config-in-db/*.toml` glob used by the existing `live-tests-config-in-database`
//! job so both paths exercise the same fixture.

use std::collections::HashMap;

use reqwest::Client;
use tensorzero_core::config::ConfigFileGlob;
use tensorzero_core::config::UninitializedConfig;
use tensorzero_core::config::editable::config_to_toml;

use crate::db_only_boot::bootstrap::bootstrap_gateway_with_toml;

/// Default fixture glob, matching the file list the
/// `live-tests-config-in-database` flavor feeds to `--migrate-config`.
/// Keeping them in sync means the `rest-bootstrap` flavor asserts the same
/// functions/models/tools/etc. are reachable as the migrate-config flavor.
const DEFAULT_FIXTURE_GLOB: &str =
    "tensorzero-core/tests/e2e/config-in-db/{tensorzero,postgres}.*.toml";

/// Env var consulted to override the fixture glob. Mostly for local iteration;
/// CI relies on the default.
const FIXTURE_GLOB_ENV: &str = "TENSORZERO_REST_BOOTSTRAP_CONFIG_PATH";

/// Build the canonical TOML + `path_contents` the `/apply` endpoint expects
/// from a glob of fixture TOMLs. This mirrors what the `--migrate-config`
/// path does internally — read glob, deserialize into `UninitializedConfig`,
/// re-serialize through `config_to_toml` — except we stop before the DB
/// write and hand the payload to the gateway over HTTP instead.
fn load_canonical_payload(glob_str: &str) -> (String, HashMap<String, String>) {
    let glob = ConfigFileGlob::new(glob_str.to_string()).expect("fixture glob should be valid");
    let globbed =
        UninitializedConfig::read_toml_config(&glob, false).expect("fixture TOMLs should parse");
    let uninit = UninitializedConfig::try_from(globbed.table)
        .expect("fixture TOMLs should deserialize into UninitializedConfig");
    config_to_toml(&uninit).expect("fixture config should re-serialize to canonical TOML")
}

/// Install the fixture config into the running gateway via
/// `POST /internal/config_toml/apply`. Marked `#[ignore]` so it doesn't run
/// as part of the normal suite — see module-level docs.
#[tokio::test]
#[ignore = "invoked explicitly by the rest-bootstrap CI job, not part of the default e2e run"]
async fn rest_bootstrap_fixture_config() {
    let glob_str =
        std::env::var(FIXTURE_GLOB_ENV).unwrap_or_else(|_| DEFAULT_FIXTURE_GLOB.to_string());
    let (toml, path_contents) = load_canonical_payload(&glob_str);

    let client = Client::new();
    let bootstrapped = bootstrap_gateway_with_toml(&client, &toml, &path_contents).await;
    assert!(
        !bootstrapped.hash.is_empty(),
        "apply should return a non-empty config hash"
    );
}
