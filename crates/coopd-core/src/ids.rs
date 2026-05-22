//! Identity types for Coop entities.
//!
//! - [`CoopId`] — a farm's network identity, e.g. `alice.coop`.
//! - [`HenId`] — an agent's globally-unique ID: `<coop_id>/<name>`.
//! - [`RoostId`] — a node identifier, unique within a Coop.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::error::{CoreError, Result};

/// Coop (farm) identity. Either a DNS hostname (`alice.coop`) or `ip:port`.
///
/// Validation rules:
/// - DNS form: lowercase, RFC 1123 hostname, ≤ 253 chars.
/// - `ip:port` form: numeric IPv4/IPv6 + port (for local/intranet farms).
#[derive(Debug, Clone, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CoopId(String);

impl CoopId {
    /// Construct a new `CoopId`, validating syntax.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::InvalidId`] if `s` is empty, longer than 253 chars,
    /// contains uppercase letters, or contains characters outside
    /// `[a-z0-9.\-:\[\]]`.
    pub fn new(s: impl Into<String>) -> Result<Self> {
        let s = s.into();
        validate_coop_id(&s)?;
        Ok(Self(s))
    }

    /// The underlying string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CoopId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for CoopId {
    type Err = CoreError;
    fn from_str(s: &str) -> Result<Self> {
        Self::new(s)
    }
}

fn validate_coop_id(s: &str) -> Result<()> {
    if s.is_empty() || s.len() > 253 {
        return Err(CoreError::InvalidId(format!("coop_id length: {}", s.len())));
    }
    // Allow either DNS hostname or ip:port. Both must be lowercase.
    if s != s.to_ascii_lowercase() {
        return Err(CoreError::InvalidId(format!(
            "coop_id must be lowercase: {s}"
        )));
    }
    // Each char must be alphanumeric, dot, dash, or colon (for port) or bracket (IPv6).
    let valid = s
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':' | '[' | ']'));
    if !valid {
        return Err(CoreError::InvalidId(format!("coop_id invalid chars: {s}")));
    }
    Ok(())
}

/// Hen (agent) identity. Format: `<coop_id>/<hen-name>`.
///
/// Validation rules for the `name` part:
/// - Lowercase ASCII letters / digits / hyphens.
/// - Must start with a letter.
/// - Length 1..=32.
#[derive(Debug, Clone, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct HenId(String);

impl HenId {
    /// Construct from a CoopId + name.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::InvalidId`] if `name` violates the hen-name
    /// validation rules (lowercase, alphanumeric + hyphens, must start with
    /// a letter, length 1..=32).
    pub fn new(coop: &CoopId, name: &str) -> Result<Self> {
        validate_hen_name(name)?;
        Ok(Self(format!("{coop}/{name}")))
    }

    /// Parse from full string `<coop_id>/<name>`.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::InvalidId`] if the string does not contain a `/`,
    /// or if either side fails its respective validation
    /// ([`CoopId::new`] for the prefix, hen-name rules for the suffix).
    pub fn parse(s: &str) -> Result<Self> {
        let (coop, name) = s
            .split_once('/')
            .ok_or_else(|| CoreError::InvalidId(format!("hen_id missing '/': {s}")))?;
        let coop = CoopId::new(coop)?;
        validate_hen_name(name)?;
        Ok(Self(format!("{coop}/{name}")))
    }

    /// The underlying full string.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Just the hen name portion (after the `/`).
    pub fn name(&self) -> &str {
        self.0.split_once('/').map_or(&self.0, |(_, n)| n)
    }

    /// Just the coop portion.
    pub fn coop(&self) -> &str {
        self.0.split_once('/').map_or(&self.0, |(c, _)| c)
    }
}

impl fmt::Display for HenId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for HenId {
    type Err = CoreError;
    fn from_str(s: &str) -> Result<Self> {
        Self::parse(s)
    }
}

fn validate_hen_name(name: &str) -> Result<()> {
    if !(1..=32).contains(&name.len()) {
        return Err(CoreError::InvalidId(format!(
            "hen name length: {}",
            name.len()
        )));
    }
    let mut chars = name.chars();
    let first = chars.next().unwrap();
    if !first.is_ascii_lowercase() {
        return Err(CoreError::InvalidId(format!(
            "hen name must start with lowercase letter: {name}"
        )));
    }
    for c in chars {
        if !(c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
            return Err(CoreError::InvalidId(format!(
                "hen name invalid char: {name}"
            )));
        }
    }
    Ok(())
}

/// Roost (node) identity. Unique within a Coop.
///
/// Validation: same rules as hen name.
#[derive(Debug, Clone, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RoostId(String);

impl RoostId {
    /// Construct a new `RoostId`, validating syntax.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::InvalidId`] if `s` violates the hen-name
    /// validation rules (lowercase alphanumeric + hyphens, must start with a
    /// letter, length 1..=32).
    pub fn new(s: impl Into<String>) -> Result<Self> {
        let s = s.into();
        validate_hen_name(&s)
            .map_err(|_| CoreError::InvalidId(format!("invalid roost_id: {s}")))?;
        Ok(Self(s))
    }

    /// The underlying string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RoostId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for RoostId {
    type Err = CoreError;
    fn from_str(s: &str) -> Result<Self> {
        Self::new(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coop_id_valid() {
        assert!(CoopId::new("alice.coop").is_ok());
        assert!(CoopId::new("192.168.1.5:9700").is_ok());
        assert!(CoopId::new("foo-bar.example.com").is_ok());
    }

    #[test]
    fn coop_id_invalid() {
        assert!(CoopId::new("").is_err());
        assert!(CoopId::new("ALICE.COOP").is_err()); // uppercase
        assert!(CoopId::new("bad space").is_err());
    }

    #[test]
    fn hen_id_construct() {
        let coop = CoopId::new("alice.coop").unwrap();
        let id = HenId::new(&coop, "aria").unwrap();
        assert_eq!(id.as_str(), "alice.coop/aria");
        assert_eq!(id.name(), "aria");
        assert_eq!(id.coop(), "alice.coop");
    }

    #[test]
    fn hen_id_parse() {
        let id = HenId::parse("bob.coop/code-reviewer-v2").unwrap();
        assert_eq!(id.name(), "code-reviewer-v2");
    }

    #[test]
    fn hen_id_invalid() {
        assert!(HenId::parse("nosep").is_err());
        let coop = CoopId::new("alice.coop").unwrap();
        assert!(HenId::new(&coop, "").is_err());
        assert!(HenId::new(&coop, "123start").is_err()); // starts with digit
        assert!(HenId::new(&coop, "UPPER").is_err());
        assert!(HenId::new(&coop, &"x".repeat(33)).is_err());
    }

    #[test]
    fn roost_id_valid() {
        assert!(RoostId::new("garage-mac").is_ok());
        assert!(RoostId::new("pi-zero-attic-01").is_ok());
    }
}
