use num_bigint::BigUint;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Which hash function produced this `SnapshotHash`.
///
/// The legacy scheme on `main` hashes canonical-TOML bytes; the canonical
/// scheme hashes the structural JSON form via
/// `StoredConfig::canonical_hash`. Both are 256-bit Blake3 outputs but
/// they are NOT interchangeable identifiers — the same logical config
/// produces different bytes under each scheme.
///
/// Snapshots persist both hashes in different columns of
/// `tensorzero.config_snapshots` (`hash` and `canonical_hash`). The
/// scheme tag carried by `SnapshotHash` tells the lookup code which
/// column to query.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SnapshotHashScheme {
    /// V1: blake3 over canonical-TOML bytes. The hash basis on `main` and
    /// what `inferences.snapshot_hash` references for every row written
    /// before the canonical-hash migration. Display prefix: `v1:`.
    LegacyToml,
    /// V2: structural blake3 over the canonical JSON Value form. Stable
    /// across serialization roundtrips (see
    /// `tensorzero_core::config::snapshot::canonical_hash`). Display
    /// prefix: `v2:`.
    Canonical,
}

impl SnapshotHashScheme {
    /// Stable string prefix used in the display, parse, and serde forms.
    /// Going forward, every hash carries its prefix on the wire so the
    /// reader can route to the correct column.
    pub const fn prefix(self) -> &'static str {
        match self {
            SnapshotHashScheme::LegacyToml => "v1",
            SnapshotHashScheme::Canonical => "v2",
        }
    }
}

/// A snapshot hash that stores both the decimal string representation
/// and the big-endian bytes for efficient storage in different databases.
///
/// As of the canonical-hash migration, every `SnapshotHash` also carries
/// a `SnapshotHashScheme` describing which hash function produced its
/// bytes. The `Display`, `FromStr`, and `Serialize`/`Deserialize` impls
/// use the prefixed form (`v1:` / `v2:`) so callers can route lookups
/// to the right column without out-of-band scheme information.
///
/// Backwards compatibility: `FromStr` (and therefore `Deserialize`)
/// accepts the legacy unprefixed decimal form and defaults it to
/// `LegacyToml`. This keeps every pre-migration `inferences.snapshot_hash`
/// value parseable without a backfill.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SnapshotHash {
    scheme: SnapshotHashScheme,
    /// The decimal string representation of the hash (used for ClickHouse)
    decimal_str: Arc<str>,
    /// The big-endian bytes representation of the hash (used for Postgres BYTEA)
    /// This is 256 bits (32 bytes).
    bytes: Arc<[u8]>,
}

impl SnapshotHash {
    /// Creates a new `SnapshotHash` from a `BigUint`, defaulting to the
    /// legacy TOML-bytes scheme. New call-sites that hash via
    /// `StoredConfig::canonical_hash` should use
    /// `from_biguint_canonical` instead.
    pub fn from_biguint(big_int: BigUint) -> Self {
        Self::from_biguint_with_scheme(big_int, SnapshotHashScheme::LegacyToml)
    }

    /// Creates a `SnapshotHash` carrying the canonical scheme tag.
    pub fn from_biguint_canonical(big_int: BigUint) -> Self {
        Self::from_biguint_with_scheme(big_int, SnapshotHashScheme::Canonical)
    }

    fn from_biguint_with_scheme(big_int: BigUint, scheme: SnapshotHashScheme) -> Self {
        let decimal_str = Arc::from(big_int.to_string());
        let bytes = Arc::from(big_int.to_bytes_be());
        Self {
            scheme,
            decimal_str,
            bytes,
        }
    }

    /// Creates a SnapshotHash from big-endian bytes, tagged as legacy.
    ///
    /// This is what `sqlx::Decode` uses for the `hash BYTEA` column. The
    /// `canonical_hash BYTEA` column read path constructs via
    /// `from_canonical_bytes` instead so the resulting `SnapshotHash`
    /// carries the right scheme tag.
    pub fn from_bytes(bytes: &[u8]) -> Self {
        Self::from_bytes_with_scheme(bytes, SnapshotHashScheme::LegacyToml)
    }

    /// Creates a `SnapshotHash` from big-endian bytes, tagged as canonical.
    pub fn from_canonical_bytes(bytes: &[u8]) -> Self {
        Self::from_bytes_with_scheme(bytes, SnapshotHashScheme::Canonical)
    }

    fn from_bytes_with_scheme(bytes: &[u8], scheme: SnapshotHashScheme) -> Self {
        let big_int = BigUint::from_bytes_be(bytes);
        Self::from_biguint_with_scheme(big_int, scheme)
    }

    /// Returns this hash's scheme tag.
    pub fn scheme(&self) -> SnapshotHashScheme {
        self.scheme
    }

    /// Returns the big-endian bytes representation.
    /// This is used for storing in Postgres as BYTEA.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Returns the lowercase hex representation of the hash.
    /// This matches the format used by ClickHouse `lower(hex(...))` and Postgres `encode(..., 'hex')`.
    ///
    /// The hex representation does NOT include the scheme prefix — it's
    /// the raw byte form intended for DB hex encodings, not for
    /// self-describing identifiers in transport.
    pub fn to_hex_string(&self) -> String {
        hex::encode(&*self.bytes)
    }

    /// Returns the decimal string form WITHOUT the scheme prefix.
    /// Intended for ClickHouse `toUInt256(...)` literals where the column
    /// type itself constrains the scheme.
    pub fn to_decimal_string(&self) -> &str {
        &self.decimal_str
    }
}

impl std::fmt::Display for SnapshotHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Always prefix the display form with the scheme. This is the
        // self-describing identifier callers pass around — URLs, log
        // lines, REST bodies — so the receiving side can route to the
        // correct column without OOB context.
        write!(f, "{}:{}", self.scheme.prefix(), self.decimal_str)
    }
}

impl Serialize for SnapshotHash {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        // Mirrors `Display`: prefixed form on the wire.
        serializer.collect_str(self)
    }
}

impl std::str::FromStr for SnapshotHash {
    type Err = num_bigint::ParseBigIntError;

    /// Accepts:
    /// - `"v1:DECIMAL"` → `LegacyToml`
    /// - `"v2:DECIMAL"` → `Canonical`
    /// - `"DECIMAL"`    → `LegacyToml` (legacy unprefixed form, kept for
    ///   backward compat with rows written before prefixing began)
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if let Some(rest) = s.strip_prefix("v1:") {
            let big_int = rest.parse::<BigUint>()?;
            Ok(SnapshotHash::from_biguint_with_scheme(
                big_int,
                SnapshotHashScheme::LegacyToml,
            ))
        } else if let Some(rest) = s.strip_prefix("v2:") {
            let big_int = rest.parse::<BigUint>()?;
            Ok(SnapshotHash::from_biguint_with_scheme(
                big_int,
                SnapshotHashScheme::Canonical,
            ))
        } else {
            // Unprefixed: legacy decimal form. Treat as LegacyToml.
            let big_int = s.parse::<BigUint>()?;
            Ok(SnapshotHash::from_biguint_with_scheme(
                big_int,
                SnapshotHashScheme::LegacyToml,
            ))
        }
    }
}

impl<'de> Deserialize<'de> for SnapshotHash {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        s.parse::<SnapshotHash>().map_err(serde::de::Error::custom)
    }
}

/// Maps `SnapshotHash` to Postgres BYTEA so it can be used directly in
/// `push_bind` and `FromRow` without manual `as_bytes()`/`from_bytes()` conversion.
impl sqlx::Type<sqlx::Postgres> for SnapshotHash {
    fn type_info() -> sqlx::postgres::PgTypeInfo {
        <Vec<u8> as sqlx::Type<sqlx::Postgres>>::type_info()
    }

    fn compatible(ty: &sqlx::postgres::PgTypeInfo) -> bool {
        <Vec<u8> as sqlx::Type<sqlx::Postgres>>::compatible(ty)
    }
}

impl sqlx::Encode<'_, sqlx::Postgres> for SnapshotHash {
    fn encode_by_ref(
        &self,
        buf: &mut sqlx::postgres::PgArgumentBuffer,
    ) -> Result<sqlx::encode::IsNull, sqlx::error::BoxDynError> {
        <&[u8] as sqlx::Encode<'_, sqlx::Postgres>>::encode_by_ref(&self.as_bytes(), buf)
    }
}

impl<'r> sqlx::Decode<'r, sqlx::Postgres> for SnapshotHash {
    /// Defaults to `LegacyToml` because the bytes alone cannot tell us
    /// which scheme produced them. Code paths reading the
    /// `canonical_hash` column should call `from_canonical_bytes`
    /// explicitly on the row's bytes.
    fn decode(value: sqlx::postgres::PgValueRef<'r>) -> Result<Self, sqlx::error::BoxDynError> {
        let bytes = <Vec<u8> as sqlx::Decode<'r, sqlx::Postgres>>::decode(value)?;
        Ok(SnapshotHash::from_bytes(&bytes))
    }
}

#[cfg(any(test, feature = "e2e_tests"))]
impl SnapshotHash {
    /// Creates a test SnapshotHash by hashing an empty input with blake3.
    /// This produces a deterministic hash suitable for testing.
    pub fn new_test() -> SnapshotHash {
        let hash = blake3::hash(&[]);
        let big_int = BigUint::from_bytes_be(hash.as_bytes());
        SnapshotHash::from_biguint(big_int)
    }
}

#[cfg(any(test, feature = "e2e_tests"))]
impl Default for SnapshotHash {
    fn default() -> Self {
        SnapshotHash::new_test()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn display_includes_scheme_prefix() {
        let legacy = SnapshotHash::from_bytes(&[0xAB; 32]);
        assert!(legacy.to_string().starts_with("v1:"));

        let canonical = SnapshotHash::from_canonical_bytes(&[0xAB; 32]);
        assert!(canonical.to_string().starts_with("v2:"));
    }

    #[test]
    fn legacy_and_canonical_with_same_bytes_are_distinguishable_by_scheme() {
        let bytes = [0x42u8; 32];
        let legacy = SnapshotHash::from_bytes(&bytes);
        let canonical = SnapshotHash::from_canonical_bytes(&bytes);

        // Same bytes...
        assert_eq!(legacy.as_bytes(), canonical.as_bytes());
        // ...but different scheme tags...
        assert_eq!(legacy.scheme(), SnapshotHashScheme::LegacyToml);
        assert_eq!(canonical.scheme(), SnapshotHashScheme::Canonical);
        // ...so they don't compare equal (scheme is part of the identity).
        assert_ne!(legacy, canonical);
        // ...and they print differently.
        assert_ne!(legacy.to_string(), canonical.to_string());
    }

    #[test]
    fn from_str_round_trip_for_both_schemes() {
        for scheme in [
            SnapshotHashScheme::LegacyToml,
            SnapshotHashScheme::Canonical,
        ] {
            let original = SnapshotHash::from_bytes_with_scheme(&[0x12; 32], scheme);
            let s = original.to_string();
            let parsed = SnapshotHash::from_str(&s).expect("parse back");
            assert_eq!(parsed, original, "round-trip for {scheme:?}");
            assert_eq!(parsed.scheme(), scheme);
        }
    }

    #[test]
    fn unprefixed_decimal_parses_as_legacy() {
        // Backwards-compat path: an `inferences.snapshot_hash` column on a
        // pre-migration row stores the decimal form WITHOUT a prefix.
        // Parsing it must yield a legacy-scheme hash.
        let bytes = [0xCD; 32];
        let big_int = BigUint::from_bytes_be(&bytes);
        let raw_decimal = big_int.to_string();

        let parsed = SnapshotHash::from_str(&raw_decimal).expect("legacy decimal parses");
        assert_eq!(parsed.scheme(), SnapshotHashScheme::LegacyToml);
        assert_eq!(parsed.as_bytes(), &bytes);
    }

    #[test]
    fn serde_round_trip_preserves_scheme() {
        let canonical = SnapshotHash::from_canonical_bytes(&[0x88; 32]);
        let json = serde_json::to_string(&canonical).expect("serialize");
        // Wire form is the prefixed string.
        assert!(json.contains("v2:"));
        let back: SnapshotHash = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, canonical);
        assert_eq!(back.scheme(), SnapshotHashScheme::Canonical);
    }
}
