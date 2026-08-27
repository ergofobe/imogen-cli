//! The small amount of date handling a client can honestly do.
//!
//! The contract is ISO-8601 and nothing else, so nothing here reformats a timestamp the
//! server sent. These functions only widen what a person may type on the command line —
//! `2024-06-01` for a whole day — into the instant the API expects.

use anyhow::{Context, Result};
use time::format_description::well_known::Rfc3339;
use time::{Date, OffsetDateTime, UtcOffset};

/// A date given as a filter means the whole day, so `--after 2024-06-01` includes things
/// taken that morning.
pub fn to_start_of_day(input: &str) -> String {
    widen(input, "T00:00:00.000Z")
}

pub fn to_end_of_day(input: &str) -> String {
    widen(input, "T23:59:59.999Z")
}

fn widen(input: &str, suffix: &str) -> String {
    let trimmed = input.trim();
    if trimmed.contains('T') {
        return trimmed.to_string();
    }
    match trimmed.len() {
        // A bare year or year-month is a range too: 2024 means all of 2024.
        4 => format!("{trimmed}-01-01{suffix}"),
        7 if suffix.starts_with("T00") => format!("{trimmed}-01{suffix}"),
        7 => format!("{trimmed}-{}{suffix}", last_day_of(trimmed)),
        10 => format!("{trimmed}{suffix}"),
        _ => trimmed.to_string(),
    }
}

fn last_day_of(year_month: &str) -> String {
    let Some((year, month)) = year_month.split_once('-') else {
        return "28".into();
    };
    let (Ok(year), Ok(month)) = (year.parse::<i32>(), month.parse::<u8>()) else {
        return "28".into();
    };
    let Ok(month) = time::Month::try_from(month) else {
        return "28".into();
    };
    format!("{:02}", month.length(year))
}

/// A capture time somebody typed, as the instant the API wants. A bare date becomes noon
/// rather than midnight: a photograph with an unknown time sorts among that day's others
/// instead of ahead of all of them.
pub fn to_timestamp(input: &str) -> Result<String> {
    let trimmed = input.trim();
    if trimmed.contains('T') {
        // Already an instant. Re-encode so a missing offset does not reach the server.
        let parsed = OffsetDateTime::parse(trimmed, &Rfc3339)
            .with_context(|| format!("{trimmed} is not an ISO-8601 timestamp"))?;
        return Ok(parsed.to_offset(UtcOffset::UTC).format(&Rfc3339)?);
    }
    let format = time::macros::format_description!("[year]-[month]-[day]");
    let date = Date::parse(trimmed, &format).with_context(|| format!("{trimmed} is not a date"))?;
    Ok(date
        .with_hms_milli(12, 0, 0, 0)?
        .assume_utc()
        .format(&Rfc3339)?)
}

/// Seconds since the epoch — how a Google Takeout sidecar states a capture time.
pub fn from_unix_seconds(seconds: i64) -> Result<String> {
    Ok(OffsetDateTime::from_unix_timestamp(seconds)
        .context("That timestamp is outside the range of a date")?
        .format(&Rfc3339)?)
}

/// `2024`, `06`, `01` out of an ISO-8601 timestamp, for a download layout. Anything that
/// is not the promised shape yields empty parts rather than a wrong date.
pub fn parts(timestamp: &str) -> (String, String, String) {
    let date = timestamp.split('T').next().unwrap_or_default();
    let mut pieces = date.split('-');
    (
        pieces.next().unwrap_or_default().to_string(),
        pieces.next().unwrap_or_default().to_string(),
        pieces.next().unwrap_or_default().to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_date_filter_covers_the_whole_day() {
        assert_eq!(to_start_of_day("2024-06-01"), "2024-06-01T00:00:00.000Z");
        assert_eq!(to_end_of_day("2024-06-01"), "2024-06-01T23:59:59.999Z");
    }

    #[test]
    fn a_month_ends_on_its_own_last_day() {
        assert_eq!(to_end_of_day("2024-02"), "2024-02-29T23:59:59.999Z");
        assert_eq!(to_end_of_day("2023-02"), "2023-02-28T23:59:59.999Z");
        assert_eq!(to_start_of_day("2024-02"), "2024-02-01T00:00:00.000Z");
    }

    #[test]
    fn an_instant_is_left_alone() {
        assert_eq!(
            to_start_of_day("2024-06-01T09:30:00.000Z"),
            "2024-06-01T09:30:00.000Z"
        );
    }

    #[test]
    fn a_typed_date_becomes_midday_so_it_sorts_among_that_day() {
        assert_eq!(to_timestamp("2024-06-01").unwrap(), "2024-06-01T12:00:00Z");
    }

    #[test]
    fn unix_seconds_become_iso() {
        assert_eq!(
            from_unix_seconds(1717233000).unwrap(),
            "2024-06-01T09:10:00Z"
        );
    }

    #[test]
    fn layout_parts_survive_a_timestamp_that_is_not_iso() {
        assert_eq!(
            parts("2024-06-01T09:30:00.000Z"),
            ("2024".into(), "06".into(), "01".into())
        );
        assert_eq!(parts("").2, "");
    }
}
