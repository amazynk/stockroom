//! Stockroom's identity: its name, app ID and catalog URL, kept in one place.
//!
//! The owner chose the name on 2026-11-20; until then it was the working name "Suite", which
//! the crates (`suite-*`) still carry.

/// Product name shown to people.
pub const NAME: &str = "Stockroom";

/// Lower-case name used for folders, binaries and scheduler entries.
pub const SLUG: &str = "stockroom";

/// Reverse-DNS application ID (desktop file, `LaunchAgent` label, `AppUserModelID`).
pub const APP_ID: &str = "com.amazynk.Stockroom";

/// Publisher shown in the Windows "Apps" list.
pub const PUBLISHER: &str = "amazynk";

/// Where the catalog lives. Stockroom takes what this address serves over HTTPS; the
/// downloads it lists are still checked against the checksums in it.
pub const CATALOG_URL: &str = "https://amazynk.com/stockroom/catalog.json";

/// The `User-Agent` sent with every request: the same for everyone, no IDs.
pub const USER_AGENT: &str = concat!("Stockroom/", env!("CARGO_PKG_VERSION"));

/// Name of the scheduled update job (systemd unit stem, schtasks name, `LaunchAgent` label).
pub const UPDATE_JOB: &str = "com.amazynk.Stockroom.update";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_consistent() {
        assert!(APP_ID.ends_with(NAME));
        assert!(UPDATE_JOB.starts_with(APP_ID));
        assert_eq!(SLUG, NAME.to_lowercase());
        assert!(CATALOG_URL.starts_with("https://"));
    }
}
