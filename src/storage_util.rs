//! Small shared helpers.

use anyhow::{bail, Result};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

pub const GIB: f64 = 1_073_741_824.0;

/// Bytes as GiB. Every size printed by this tool uses GiB (÷ 1,073,741,824).
pub fn gib(bytes: u64) -> f64 {
    bytes as f64 / GIB
}

/// Split an `OWNER/NAME` argument.
pub fn parse_repo(arg: &str) -> Result<(String, String)> {
    match arg.split_once('/') {
        Some((owner, name)) if !owner.is_empty() && !name.is_empty() && !name.contains('/') => {
            Ok((owner.to_string(), name.to_string()))
        }
        _ => bail!("expected OWNER/NAME, got {arg:?}"),
    }
}

/// Parse an RFC 3339 timestamp (as returned by the REST API) to a Unix epoch.
pub fn epoch_seconds(timestamp: &str) -> Option<i64> {
    OffsetDateTime::parse(timestamp, &Rfc3339)
        .ok()
        .map(|t| t.unix_timestamp())
}

/// Current Unix epoch in seconds.
pub fn now_epoch() -> i64 {
    OffsetDateTime::now_utc().unix_timestamp()
}
