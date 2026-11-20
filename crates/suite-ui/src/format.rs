//! Short human-readable numbers and times, with the shown language's decimal separator.

use time::OffsetDateTime;

use crate::i18n::{self, tr};

const BYTE_UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];

#[expect(clippy::cast_precision_loss, reason = "display only")]
fn scaled(value: u64) -> (f64, usize) {
    let mut v = value as f64;
    let mut unit = 0;
    while v >= 1000.0 && unit < BYTE_UNITS.len() - 1 {
        v /= 1000.0;
        unit += 1;
    }
    (v, unit)
}

/// `value` with `places` decimals and the language's separator.
pub(crate) fn fixed(value: f64, places: usize) -> String {
    let text = format!("{value:.places$}");
    match i18n::decimal() {
        '.' => text,
        sep => text.replace('.', sep.encode_utf8(&mut [0; 4])),
    }
}

/// A version as people read it: without build metadata (`2026.11.19+build.091ee20dff0c`,
/// which a download without a version of its own gets, is shown as `2026.11.19`).
pub(crate) fn version(v: &semver::Version) -> String {
    let mut shown = v.clone();
    shown.build = semver::BuildMetadata::EMPTY;
    shown.to_string()
}

/// "512 B", "41 MB", "5.8 GB".
pub(crate) fn bytes(value: u64) -> String {
    let (v, unit) = scaled(value);
    format!("{} {}", fixed(v, usize::from(unit >= 3 || (unit > 0 && v < 10.0))), BYTE_UNITS[unit])
}

/// Bytes per second.
pub(crate) fn rate(per_sec: f64) -> String {
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "display only")]
    let (v, unit) = scaled(per_sec.max(0.0) as u64);
    format!("{} {}/s", fixed(v, usize::from(v < 10.0 && unit > 0)), BYTE_UNITS[unit])
}

/// A time span: "45 s", "3 min", "1.5 h".
#[expect(clippy::cast_precision_loss, reason = "display only")]
pub(crate) fn duration(secs: u64) -> String {
    match secs {
        0..60 => tr!("{} s", secs.max(1)),
        60..3600 => tr!("{} min", (secs + 30) / 60),
        _ => tr!("{} h", fixed(secs as f64 / 3600.0, 1)),
    }
}

/// A calendar date in the local time zone: "2026-11-12".
pub(crate) fn date(at: OffsetDateTime) -> String {
    let local = local(at);
    local.format("%Y-%m-%d").to_string()
}

/// "Today, 14:05", "Yesterday, 09:10", "2026-11-12, 18:00", relative to `now`.
pub(crate) fn when(at: OffsetDateTime, now: OffsetDateTime) -> String {
    let (at, now) = (local(at), local(now));
    let clock = at.format("%H:%M").to_string();
    let days = (now.date_naive() - at.date_naive()).num_days();
    match days {
        0 => tr!("Today, {}", clock),
        1 => tr!("Yesterday, {}", clock),
        _ => format!("{}, {clock}", at.format("%Y-%m-%d")),
    }
}

fn local(at: OffsetDateTime) -> chrono::DateTime<chrono::Local> {
    let utc = chrono::DateTime::from_timestamp(at.unix_timestamp(), 0).unwrap_or_default();
    utc.with_timezone(&chrono::Local)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_without_build_metadata() {
        let v = |s: &str| version(&s.parse().unwrap());
        assert_eq!(v("2026.11.19+build.091ee20dff0c"), "2026.11.19");
        assert_eq!(v("1.2.0-beta.1+7"), "1.2.0-beta.1");
        assert_eq!(v("0.1.2"), "0.1.2");
    }

    #[test]
    fn sizes() {
        assert_eq!(bytes(512), "512 B");
        assert_eq!(bytes(4_200_000), "4.2 MB");
        assert_eq!(bytes(41_000_000), "41 MB");
        assert_eq!(bytes(5_800_000_000), "5.8 GB");
    }

    #[test]
    fn rates_and_durations() {
        assert_eq!(rate(61_000_000.0), "61 MB/s");
        assert_eq!(rate(9_200_000.0), "9.2 MB/s");
        assert_eq!(rate(800.0), "800 B/s");
        assert_eq!(duration(0), "1 s");
        assert_eq!(duration(45), "45 s");
        assert_eq!(duration(600), "10 min");
        assert_eq!(duration(5400), "1.5 h");
    }

    #[test]
    fn german_separator() {
        i18n::use_catalog("de");
        assert_eq!(fixed(1.5, 1), "1,5");
        i18n::use_catalog("en");
        assert_eq!(fixed(1.5, 1), "1.5");
    }

    #[test]
    fn relative_days() {
        let now = OffsetDateTime::now_utc();
        assert!(when(now, now).starts_with("Today, "));
        assert!(when(now - time::Duration::days(1), now).starts_with("Yesterday, "));
        let old = when(now - time::Duration::days(30), now);
        assert!(old.contains(&date(now - time::Duration::days(30))), "{old}");
    }
}
