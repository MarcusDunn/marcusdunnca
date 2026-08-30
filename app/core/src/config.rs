//! Environment reading.
//!
//! Everything here is called once, at cold start, and any failure aborts the
//! function before it serves a request. That is deliberate: a Lambda missing
//! `TABLE_NAME` that starts anyway and fails on the third route is much harder
//! to diagnose than one that never reports healthy.

use crate::error::{Error, Result};

/// A required variable. Absent or empty is an error — an empty string is
/// almost always a Terraform interpolation that resolved to nothing, and
/// treating it as a valid value produces a function that queries a table named
/// `""`.
pub fn require(name: &str) -> Result<String> {
    match std::env::var(name) {
        Ok(v) if !v.trim().is_empty() => Ok(v),
        Ok(_) => Err(Error::Config(format!("{name} is set but empty"))),
        Err(_) => Err(Error::Config(format!("{name} is not set"))),
    }
}

/// An optional variable with a numeric default.
///
/// A *malformed* value is an error rather than a silent fall back to the
/// default: `MAX_PAGES=1oo` should stop the deploy, not quietly restore a limit
/// someone was trying to change.
pub fn parse_or<T>(name: &str, default: T) -> Result<T>
where
    T: std::str::FromStr,
{
    match std::env::var(name) {
        Err(_) => Ok(default),
        Ok(v) if v.trim().is_empty() => Ok(default),
        Ok(v) => v
            .trim()
            .parse()
            .map_err(|_| Error::Config(format!("{name} is not a valid value: {v:?}"))),
    }
}
