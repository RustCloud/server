//! When events happen: their occurrences in a range of time, with repeats
//! (`RRULE`), skipped dates (`EXDATE`) and changed occurrences
//! (`RECURRENCE-ID`) taken into account.
//!
//! Times leave as text for the page: `2026-10-29` for all-day events,
//! `2026-10-29T08:00:00Z` for exact moments (the browser shows them in its
//! own time zone), `2026-10-29T09:00:00` for floating times.

use chrono::{DateTime, Duration, NaiveDateTime, TimeZone, Utc};
use rrule::{RRule, Tz, Unvalidated};

use crate::ical::{Event, Time};

/// Most occurrences of one event in one range.
const MAX_OCCURRENCES: u16 = 500;

/// One time an event happens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Occurrence {
    pub start: String,
    pub end: String,
    pub all_day: bool,
}

/// The occurrences of `event` that overlap `[from, to)`. `moved` are the
/// original times of occurrences changed in their own `VEVENT`.
pub fn occurrences(event: &Event, moved: &[DateTime<Utc>], from: DateTime<Utc>, to: DateTime<Utc>) -> Vec<Occurrence> {
    let start = instant(&event.start);
    let length = length(event);
    // Dates and floating times are in the viewer's time zone, which may be up
    // to 14 hours away from UTC: look a little wider; the page sorts it out.
    let slack = match event.start {
        Time::Date { .. } | Time::Floating { .. } => Duration::hours(14),
        _ => Duration::zero(),
    };
    let (from, to) = (from - slack, to + slack);

    let starts = match event.rrule.as_deref().and_then(|rule| repeats(event, rule, moved, from - length, to)) {
        Some(starts) => starts,
        None => vec![start],
    };
    starts
        .into_iter()
        .filter(|start| *start < to && *start + length.max(Duration::seconds(1)) > from)
        .map(|start| Occurrence {
            start: text(&event.start, start),
            end: text(&event.start, start + length),
            all_day: matches!(event.start, Time::Date { .. }),
        })
        .collect()
}

/// The start of every repeat in `[from, to)`; `None` if the rule can't be read.
fn repeats(
    event: &Event,
    rule: &str,
    moved: &[DateTime<Utc>],
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Option<Vec<DateTime<Utc>>> {
    let dt_start = zoned(&event.start);
    let rule: RRule<Unvalidated> = until_in_utc(rule, &event.start).parse().ok()?;
    let mut set = rule.build(dt_start).ok()?;
    for skipped in event.exdates.iter().map(instant).chain(moved.iter().copied()) {
        set = set.exdate(skipped.with_timezone(&Tz::UTC));
    }
    let found = set.after(from.with_timezone(&Tz::UTC)).before(to.with_timezone(&Tz::UTC)).all(MAX_OCCURRENCES);
    Some(found.dates.into_iter().map(|at| at.with_timezone(&Utc)).collect())
}

/// `UNTIL` must be in UTC when the start has a time; clients often write a date.
fn until_in_utc(rule: &str, start: &Time) -> String {
    rule.split(';')
        .map(|part| match part.split_once('=') {
            Some((key, value)) if key.eq_ignore_ascii_case("UNTIL") && value.len() == 8 => match start {
                Time::Date { .. } => format!("UNTIL={value}T000000Z"),
                _ => format!("UNTIL={value}T235959Z"),
            },
            _ => part.to_owned(),
        })
        .collect::<Vec<_>>()
        .join(";")
}

/// How long each occurrence lasts.
pub fn length(event: &Event) -> Duration {
    if let Some(end) = &event.end {
        return (instant(end) - instant(&event.start)).max(Duration::zero());
    }
    match (event.duration, &event.start) {
        (Some(seconds), _) => Duration::seconds(seconds.max(0)),
        (None, Time::Date { .. }) => Duration::days(1),
        (None, _) => Duration::zero(),
    }
}

/// A time as a moment: dates and floating times as if they were in UTC.
pub fn instant(time: &Time) -> DateTime<Utc> {
    zoned(time).with_timezone(&Utc)
}

fn zoned(time: &Time) -> DateTime<Tz> {
    match time {
        Time::Date { date } => Tz::UTC.from_utc_datetime(&date.and_hms_opt(0, 0, 0).expect("midnight exists")),
        Time::Utc { at } => at.with_timezone(&Tz::UTC),
        Time::Floating { at } => Tz::UTC.from_utc_datetime(at),
        Time::Zoned { at, tz } => match time_zone(tz) {
            Some(tz) => local(Tz::Tz(tz), at),
            None => Tz::UTC.from_utc_datetime(at),
        },
    }
}

/// Wall-clock time in a zone; in a gap (spring forward), an hour later.
fn local(tz: Tz, at: &NaiveDateTime) -> DateTime<Tz> {
    tz.from_local_datetime(at)
        .earliest()
        .or_else(|| tz.from_local_datetime(&(*at + Duration::hours(1))).earliest())
        .unwrap_or_else(|| Tz::UTC.from_utc_datetime(at))
}

/// An IANA zone from a `TZID`. Some clients prefix it, e.g.
/// `/mozilla.org/20050126_1/Europe/Berlin`: the longest known suffix wins.
pub fn time_zone(name: &str) -> Option<chrono_tz::Tz> {
    std::iter::once(name)
        .chain(name.match_indices('/').map(|(i, _)| &name[i + 1..]))
        .find_map(|candidate| candidate.parse().ok())
}

/// A moment as the page gets it, in the kind of `like`.
fn text(like: &Time, at: DateTime<Utc>) -> String {
    match like {
        Time::Date { .. } => at.format("%Y-%m-%d").to_string(),
        Time::Floating { .. } => at.format("%Y-%m-%dT%H:%M:%S").to_string(),
        Time::Utc { .. } | Time::Zoned { .. } => at.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ical;

    fn utc(text: &str) -> DateTime<Utc> {
        NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M").unwrap().and_utc()
    }

    fn event(lines: &str) -> Event {
        let text = format!("BEGIN:VCALENDAR\nBEGIN:VEVENT\nUID:x\n{lines}\nEND:VEVENT\nEND:VCALENDAR\n");
        ical::events(&text).remove(0)
    }

    fn starts(event: &Event, from: &str, to: &str) -> Vec<String> {
        occurrences(event, &[], utc(from), utc(to)).into_iter().map(|o| o.start).collect()
    }

    #[test]
    fn a_single_event_is_found_when_it_overlaps() {
        let meeting = event("DTSTART:20261029T090000Z\nDTEND:20261029T100000Z");
        assert_eq!(starts(&meeting, "2026-10-29 09:30", "2026-10-30 00:00"), ["2026-10-29T09:00:00Z"]);
        assert!(starts(&meeting, "2026-10-29 10:00", "2026-10-30 00:00").is_empty());
        let found = occurrences(&meeting, &[], utc("2026-10-01 00:00"), utc("2026-11-01 00:00"));
        assert_eq!(found[0].end, "2026-10-29T10:00:00Z");
    }

    #[test]
    fn zoned_times_follow_daylight_saving() {
        // Berlin leaves summer time on 2026-10-25: 09:00 is 07:00Z, then 08:00Z.
        let daily = event("DTSTART;TZID=Europe/Berlin:20261023T090000\nRRULE:FREQ=DAILY;COUNT=4");
        let found = starts(&daily, "2026-10-01 00:00", "2026-11-01 00:00");
        assert_eq!(
            found,
            ["2026-10-23T07:00:00Z", "2026-10-24T07:00:00Z", "2026-10-25T08:00:00Z", "2026-10-26T08:00:00Z"]
        );
    }

    #[test]
    fn repeats_skip_exdates_and_moved_occurrences() {
        let weekly = event(
            "DTSTART;TZID=Europe/Berlin:20261102T090000\nDURATION:PT1H\nRRULE:FREQ=WEEKLY;UNTIL=20261130\n\
             EXDATE;TZID=Europe/Berlin:20261109T090000",
        );
        let moved = [utc("2026-11-16 08:00")];
        let found: Vec<String> = occurrences(&weekly, &moved, utc("2026-11-01 00:00"), utc("2026-12-31 00:00"))
            .into_iter()
            .map(|o| o.start)
            .collect();
        assert_eq!(found, ["2026-11-02T08:00:00Z", "2026-11-23T08:00:00Z", "2026-11-30T08:00:00Z"]);
    }

    #[test]
    fn all_day_events_stay_dates() {
        let birthday = event("DTSTART;VALUE=DATE:20200301\nRRULE:FREQ=YEARLY");
        let found = occurrences(&birthday, &[], utc("2026-03-01 00:00"), utc("2026-03-02 00:00"));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].start, "2026-03-01");
        assert_eq!(found[0].end, "2026-03-02", "the end date is exclusive");
        assert!(found[0].all_day);
    }

    #[test]
    fn unreadable_rules_show_the_first_occurrence() {
        let broken = event("DTSTART:20261029T090000Z\nRRULE:FREQ=SOMETIMES");
        assert_eq!(starts(&broken, "2026-10-01 00:00", "2026-11-01 00:00"), ["2026-10-29T09:00:00Z"]);
    }

    #[test]
    fn prefixed_zone_names_are_understood() {
        assert_eq!(time_zone("/mozilla.org/20050126_1/Europe/Berlin"), Some(chrono_tz::Europe::Berlin));
        assert_eq!(time_zone("Europe/Moscow"), Some(chrono_tz::Europe::Moscow));
        assert_eq!(time_zone("Russian Standard Time"), None);
    }
}
