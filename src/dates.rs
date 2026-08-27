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
        // A bare year or year-month is a range too: 2024 means all of 2024, which ends on
        // the last day of December rather than the first day of January.
        4 if suffix.starts_with("T00") => format!("{trimmed}-01-01{suffix}"),
        4 => format!("{trimmed}-12-31{suffix}"),
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

/// Month names, so the browser's jump prompt takes "aug 2011" as well as "2011-08". A
/// date filter never needed these — a person typing a command line writes the ISO shape —
/// but somebody scrubbing a twenty-year timeline is thinking in months, not in hyphens.
const MONTHS: [&str; 12] = [
    "january",
    "february",
    "march",
    "april",
    "may",
    "june",
    "july",
    "august",
    "september",
    "october",
    "november",
    "december",
];

/// A date somebody typed, and how much of one they actually said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Day {
    /// The day to land on, `YYYY-MM-DD`.
    pub date: String,
    /// How many characters of `date` were named rather than filled in: 4 for a year, 7 for
    /// a month, 10 for a day. Landing anywhere inside what was named is landing where they
    /// asked, so a caller knows when it has nothing to apologise for.
    pub named: usize,
}

/// The day somebody means, or `None` when what they typed is not a date at all.
///
/// Anything less than a whole day widens to that period's *last* day, because the timeline
/// runs newest first: a jump into August 2011 should land at the top of August, not at the
/// bottom of it.
pub fn to_day(input: &str) -> Option<Day> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some((year, month)) = year_and_named_month(trimmed) {
        return Some(Day {
            date: format!(
                "{year}-{month:02}-{}",
                last_day_of(&format!("{year}-{month:02}"))
            ),
            named: 7,
        });
    }
    // Otherwise it is one of the shapes `--before` already takes, and the end of whatever
    // period it names is the day to land on.
    let widened = to_end_of_day(trimmed);
    let date = widened.split('T').next().unwrap_or_default();
    let format = time::macros::format_description!("[year]-[month]-[day]");
    Date::parse(date, &format).ok()?;
    Some(Day {
        date: date.to_string(),
        named: match trimmed.len() {
            4 => 4,
            7 => 7,
            _ => 10,
        },
    })
}

/// `aug 2011`, `August 2011`, `2011 sept`. Returns `None` for anything carrying a token
/// that is neither a four-digit year nor a month name, so an ISO date falls through to be
/// parsed properly rather than being guessed at here.
fn year_and_named_month(input: &str) -> Option<(i32, u8)> {
    let lowered = input.to_lowercase();
    let mut year = None;
    let mut month = None;
    for word in lowered
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
    {
        if word.len() == 4 && word.bytes().all(|byte| byte.is_ascii_digit()) {
            year = Some(word.parse().ok()?);
        } else if word.len() >= 3 {
            // Three letters is enough to name a month, and is what people type.
            let index = MONTHS.iter().position(|name| name.starts_with(word))?;
            month = Some(index as u8 + 1);
        } else {
            return None;
        }
    }
    Some((year?, month?))
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
    fn a_bare_year_ends_in_december() {
        // It used to end on the first of January, which made `--before 2011` mean
        // "before the second day of 2011" and quietly hid the whole year.
        assert_eq!(to_end_of_day("2011"), "2011-12-31T23:59:59.999Z");
        assert_eq!(to_start_of_day("2011"), "2011-01-01T00:00:00.000Z");
    }

    fn day(input: &str) -> Option<String> {
        to_day(input).map(|day| day.date)
    }

    #[test]
    fn a_day_to_jump_to_widens_to_the_top_of_whatever_period_was_named() {
        assert_eq!(day("2011-08-14").as_deref(), Some("2011-08-14"));
        assert_eq!(day("2011-08").as_deref(), Some("2011-08-31"));
        assert_eq!(day("2011").as_deref(), Some("2011-12-31"));
        assert_eq!(day("aug 2011").as_deref(), Some("2011-08-31"));
        assert_eq!(day("  September 2011 ").as_deref(), Some("2011-09-30"));
        assert_eq!(day("2011 feb").as_deref(), Some("2011-02-28"));
        assert_eq!(day("2012 feb").as_deref(), Some("2012-02-29"));
    }

    /// What was filled in, and what was actually said.
    #[test]
    fn a_date_remembers_how_much_of_it_was_named() {
        assert_eq!(to_day("2011").unwrap().named, 4);
        assert_eq!(to_day("2011-08").unwrap().named, 7);
        assert_eq!(to_day("aug 2011").unwrap().named, 7);
        assert_eq!(to_day("2011-08-14").unwrap().named, 10);
    }

    #[test]
    fn something_that_is_not_a_date_is_not_guessed_at() {
        assert_eq!(to_day("not a date"), None);
        assert_eq!(to_day(""), None);
        assert_eq!(to_day("2011-13-40"), None);
        assert_eq!(to_day("beach"), None);
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
