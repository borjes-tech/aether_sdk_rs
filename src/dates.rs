//! Dates, periods, working days and numbering: the calendar arithmetic business plugins keep
//! needing, with no kernel involved (WebAssembly has no clock, so "today" comes from
//! [`context::current`](crate::context::current)).
//!
//! Dates travel as `YYYY-MM-DD` text, the way the kernel stores `date` fields.

pub use chrono::{Datelike, Duration, NaiveDate, NaiveDateTime, NaiveTime, Timelike, Weekday};

use crate::error::{Error, Result};

/// A date written as `YYYY-MM-DD`.
pub fn parse_date(text: &str) -> Result<NaiveDate> {
    NaiveDate::parse_from_str(text, "%Y-%m-%d").map_err(|_| Error::msg(format!("`{text}` is not a date like 2026-10-03")))
}

/// The same date as text.
pub fn format_date(date: NaiveDate) -> String {
    date.format("%Y-%m-%d").to_string()
}

/// An optional date; an empty text counts as none.
pub fn parse_optional(text: Option<&str>) -> Result<Option<NaiveDate>> {
    match text {
        None => Ok(None),
        Some(text) if text.trim().is_empty() => Ok(None),
        Some(text) => parse_date(text).map(Some),
    }
}

/// `start` must not be after `end` (an open end is always fine).
pub fn check_order(start: NaiveDate, end: Option<NaiveDate>, what: &str) -> Result<()> {
    match end {
        Some(end) if end < start => Err(Error::msg(format!("{what}: the end ({end}) is before the start ({start})"))),
        _ => Ok(()),
    }
}

/// Whether two periods share a day. A missing end means the period never ends.
pub fn overlaps(a_start: NaiveDate, a_end: Option<NaiveDate>, b_start: NaiveDate, b_end: Option<NaiveDate>) -> bool {
    let a_ends_before_b = a_end.is_some_and(|end| end < b_start);
    let b_ends_before_a = b_end.is_some_and(|end| end < a_start);
    !(a_ends_before_b || b_ends_before_a)
}

/// The days that are not worked every week.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Calendar {
    pub weekend: Vec<Weekday>,
    pub holidays: Vec<NaiveDate>,
}

impl Default for Calendar {
    fn default() -> Self {
        Self { weekend: vec![Weekday::Sat, Weekday::Sun], holidays: Vec::new() }
    }
}

impl Calendar {
    pub fn is_working_day(&self, date: NaiveDate) -> bool {
        !self.weekend.contains(&date.weekday()) && !self.holidays.contains(&date)
    }

    /// Days actually worked from `start` to `end`, both included.
    pub fn working_days(&self, start: NaiveDate, end: NaiveDate) -> u32 {
        if end < start {
            return 0;
        }
        let mut count = 0;
        let mut day = start;
        while day <= end {
            if self.is_working_day(day) {
                count += 1;
            }
            day += Duration::days(1);
        }
        count
    }

    /// The first working day on or after `date`.
    pub fn next_working_day(&self, mut date: NaiveDate) -> NaiveDate {
        // A calendar where every day is off would never end; the week has seven days, so a
        // bounded search is enough to know.
        for _ in 0..366 {
            if self.is_working_day(date) {
                return date;
            }
            date += Duration::days(1);
        }
        date
    }
}

/// A timestamp as RFC 3339 text (`2026-10-05T08:03:00Z`, or with an offset), as an instant in UTC.
pub fn parse_datetime(text: &str) -> Result<NaiveDateTime> {
    chrono::DateTime::parse_from_rfc3339(text.trim())
        .map(|moment| moment.naive_utc())
        .map_err(|_| Error::msg(format!("`{text}` is not a timestamp like 2026-10-05T08:03:00Z")))
}

/// The same instant as RFC 3339 text in UTC, to the second.
pub fn format_datetime(moment: NaiveDateTime) -> String {
    moment.format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// A time of day written `HH:MM`.
pub fn parse_time(text: &str) -> Result<NaiveTime> {
    NaiveTime::parse_from_str(text.trim(), "%H:%M").map_err(|_| Error::msg(format!("`{text}` is not a time like 08:30")))
}

/// A holiday on file: its date, and whether it falls on the same day every year.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HolidayRow {
    pub date: NaiveDate,
    pub recurring: bool,
}

/// The concrete holiday dates that fall between two years (inclusive). A recurring holiday on
/// 29 February is only there in leap years.
pub fn holiday_dates(rows: &[HolidayRow], first_year: i32, last_year: i32) -> Vec<NaiveDate> {
    let mut dates = Vec::new();
    for row in rows {
        if row.recurring {
            for year in first_year..=last_year {
                if let Some(date) = NaiveDate::from_ymd_opt(year, row.date.month(), row.date.day()) {
                    dates.push(date);
                }
            }
        } else if (first_year..=last_year).contains(&row.date.year()) {
            dates.push(row.date);
        }
    }
    dates.sort();
    dates.dedup();
    dates
}

/// The weekdays named by short English names (`sat`, `sun`), for settings.
pub fn weekdays(names: &[String]) -> Result<Vec<Weekday>> {
    names
        .iter()
        .map(|name| name.parse::<Weekday>().map_err(|_| Error::msg(format!("`{name}` is not a weekday like mon or sunday"))))
        .collect()
}

/// `date` moved forward by whole months; a day that does not exist in the target month (31 Jan
/// plus one month) becomes that month's last day.
pub fn add_months(date: NaiveDate, months: u32) -> NaiveDate {
    date.checked_add_months(chrono::Months::new(months)).unwrap_or(date)
}

/// Completed years between `born` and `on`.
pub fn age_on(born: NaiveDate, on: NaiveDate) -> u32 {
    let mut years = on.year() - born.year();
    if (on.month(), on.day()) < (born.month(), born.day()) {
        years -= 1;
    }
    years.max(0) as u32
}

/// Complete months of service from `start` to `on`.
pub fn months_between(start: NaiveDate, on: NaiveDate) -> u32 {
    if on < start {
        return 0;
    }
    let mut months = (on.year() - start.year()) * 12 + on.month() as i32 - start.month() as i32;
    if on.day() < start.day() {
        months -= 1;
    }
    months.max(0) as u32
}

/// A number like `EMP-00042` from a prefix, a counter value and a width.
pub fn numbered(prefix: &str, number: i64, width: usize) -> String {
    format!("{prefix}{number:0width$}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Result;

    fn d(text: &str) -> NaiveDate {
        parse_date(text).unwrap_or_default()
    }

    #[test]
    fn dates_parse_and_print() {
        assert_eq!(format_date(d("2026-02-28")), "2026-02-28");
        assert!(parse_date("2026-02-30").is_err());
        assert!(parse_date("tomorrow").is_err());
        assert_eq!(parse_optional(Some("")).ok(), Some(None));
        assert_eq!(parse_optional(None).ok(), Some(None));
        assert_eq!(parse_optional(Some("2026-01-02")).ok(), Some(Some(d("2026-01-02"))));
    }

    #[test]
    fn an_end_before_the_start_is_refused() {
        assert!(check_order(d("2026-01-02"), Some(d("2026-01-01")), "contract").is_err());
        assert!(check_order(d("2026-01-02"), Some(d("2026-01-02")), "contract").is_ok());
        assert!(check_order(d("2026-01-02"), None, "contract").is_ok());
    }

    #[test]
    fn periods_overlap_when_they_share_a_day() {
        let (a, b) = (d("2026-01-01"), d("2026-01-31"));
        assert!(overlaps(a, Some(b), d("2026-01-31"), Some(d("2026-02-10"))));
        assert!(!overlaps(a, Some(b), d("2026-02-01"), None));
        assert!(overlaps(a, None, d("2030-01-01"), None));
        assert!(!overlaps(d("2026-03-01"), None, a, Some(b)));
    }

    #[test]
    fn working_days_skip_weekends_and_holidays() {
        // Monday 5 Oct 2026 to Sunday 11 Oct.
        let mut calendar = Calendar::default();
        assert_eq!(calendar.working_days(d("2026-10-05"), d("2026-10-11")), 5);
        calendar.holidays.push(d("2026-10-07"));
        assert_eq!(calendar.working_days(d("2026-10-05"), d("2026-10-11")), 4);
        assert_eq!(calendar.working_days(d("2026-10-11"), d("2026-10-05")), 0);
        assert_eq!(calendar.working_days(d("2026-10-10"), d("2026-10-11")), 0);
        assert_eq!(calendar.next_working_day(d("2026-10-10")), d("2026-10-12"));
    }

    #[test]
    fn a_custom_weekend_is_respected() {
        let calendar = Calendar { weekend: weekdays(&["fri".into(), "sat".into()]).unwrap_or_default(), holidays: vec![] };
        assert_eq!(calendar.working_days(d("2026-10-05"), d("2026-10-11")), 5);
        assert!(!calendar.is_working_day(d("2026-10-09")));
        assert!(weekdays(&["funday".into()]).is_err());
    }

    #[test]
    fn months_and_ages() {
        assert_eq!(add_months(d("2026-01-31"), 1), d("2026-02-28"));
        assert_eq!(add_months(d("2026-10-05"), 6), d("2027-04-05"));
        assert_eq!(age_on(d("1990-10-06"), d("2026-10-05")), 35);
        assert_eq!(age_on(d("1990-10-05"), d("2026-10-05")), 36);
        assert_eq!(months_between(d("2026-01-15"), d("2026-04-14")), 2);
        assert_eq!(months_between(d("2026-01-15"), d("2026-04-15")), 3);
        assert_eq!(months_between(d("2026-05-01"), d("2026-01-01")), 0);
    }

    #[test]
    fn numbers_are_padded() {
        assert_eq!(numbered("EMP-", 42, 5), "EMP-00042");
        assert_eq!(numbered("EMP-", 123456, 5), "EMP-123456");
    }

    #[test]
    fn recurring_holidays_repeat_every_year() {
        let rows = vec![
            HolidayRow { date: d("2020-12-25"), recurring: true },
            HolidayRow { date: d("2026-03-26"), recurring: false },
            HolidayRow { date: d("2020-02-29"), recurring: true },
        ];
        assert_eq!(holiday_dates(&rows, 2026, 2027), vec![d("2026-03-26"), d("2026-12-25"), d("2027-12-25")]);
        assert!(holiday_dates(&rows, 2028, 2028).contains(&d("2028-02-29")));
        assert!(!holiday_dates(&rows, 2027, 2027).contains(&d("2027-02-28")));
    }

    #[test]
    fn timestamps_and_times_of_day() -> Result<()> {
        let moment = parse_datetime("2026-10-05T08:03:00Z")?;
        assert_eq!(format_datetime(moment), "2026-10-05T08:03:00Z");
        // An offset is converted to UTC.
        assert_eq!(format_datetime(parse_datetime("2026-10-05T09:03:00+01:00")?), "2026-10-05T08:03:00Z");
        assert!(parse_datetime("yesterday").is_err());
        assert_eq!(parse_time("08:30")?.hour(), 8);
        assert!(parse_time("25:00").is_err());
        Ok(())
    }
}
