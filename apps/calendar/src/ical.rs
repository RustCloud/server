//! Just enough iCalendar (RFC 5545) for events: reading the `VEVENT`s of a
//! `.ics` file, writing a new one, and changing the fields the web page edits
//! while keeping everything else a client put there (alarms, attendees…).

use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};
use serde::{Deserialize, Serialize};

/// A moment as iCalendar writes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Time {
    /// A whole day (`VALUE=DATE`).
    Date { date: NaiveDate },
    /// An exact moment (`…Z`).
    Utc { at: DateTime<Utc> },
    /// Wall-clock time in a time zone (`TZID=…`).
    Zoned { at: NaiveDateTime, tz: String },
    /// Wall-clock time wherever the viewer is.
    Floating { at: NaiveDateTime },
}

/// What the calendar needs of a `VEVENT`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    pub uid: String,
    pub summary: String,
    pub location: String,
    pub description: String,
    pub start: Time,
    pub end: Option<Time>,
    /// `DURATION` in seconds, when there is no `DTEND`.
    pub duration: Option<i64>,
    /// The `RRULE` value, e.g. `FREQ=WEEKLY;BYDAY=MO`.
    pub rrule: Option<String>,
    pub exdates: Vec<Time>,
    /// Set on a changed occurrence of a repeating event.
    pub recurrence_id: Option<Time>,
}

/// A content line: `NAME;PARAM=VALUE:value`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Property {
    pub name: String,
    pub params: Vec<(String, String)>,
    pub value: String,
}

impl Property {
    fn param(&self, name: &str) -> Option<&str> {
        self.params.iter().find(|(key, _)| key == name).map(|(_, value)| value.as_str())
    }
}

/// The lines of a file with folded lines joined.
pub fn unfold(text: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for line in text.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        match (line.strip_prefix([' ', '\t']), lines.last_mut()) {
            (Some(rest), Some(last)) => last.push_str(rest),
            _ if line.is_empty() => {}
            _ => lines.push(line.to_owned()),
        }
    }
    lines
}

pub fn parse_line(line: &str) -> Option<Property> {
    // The value starts at the first `:` outside quotes.
    let mut quoted = false;
    let colon = line.char_indices().find_map(|(i, c)| {
        match c {
            '"' => quoted = !quoted,
            ':' if !quoted => return Some(i),
            _ => {}
        }
        None
    })?;
    let (head, value) = (&line[..colon], &line[colon + 1..]);
    let mut parts = head.split(';');
    let name = parts.next()?.trim().to_ascii_uppercase();
    let params = parts
        .filter_map(|param| param.split_once('='))
        .map(|(key, value)| (key.to_ascii_uppercase(), value.trim_matches('"').to_owned()))
        .collect();
    Some(Property { name, params, value: value.to_owned() })
}

/// The events of a file. A file without a usable `VEVENT` has none.
pub fn events(text: &str) -> Vec<Event> {
    let mut events = Vec::new();
    let mut current: Option<Vec<Property>> = None;
    // Depth inside a `VEVENT`: its `VALARM`s are skipped.
    let mut nested = 0;
    for property in unfold(text).iter().filter_map(|line| parse_line(line)) {
        match (property.name.as_str(), property.value.to_ascii_uppercase().as_str(), &mut current) {
            ("BEGIN", "VEVENT", None) => current = Some(Vec::new()),
            ("BEGIN", _, Some(_)) => nested += 1,
            ("END", "VEVENT", Some(_)) if nested == 0 => {
                events.extend(current.take().and_then(|props| event(&props)));
            }
            ("END", _, Some(_)) => nested -= 1,
            (_, _, Some(props)) if nested == 0 => props.push(property),
            _ => {}
        }
    }
    events
}

fn event(props: &[Property]) -> Option<Event> {
    let get = |name: &str| props.iter().find(|p| p.name == name);
    let text = |name: &str| get(name).map(|p| unescape(&p.value)).unwrap_or_default();
    Some(Event {
        uid: text("UID"),
        summary: text("SUMMARY"),
        location: text("LOCATION"),
        description: text("DESCRIPTION"),
        start: time(get("DTSTART")?)?,
        end: get("DTEND").and_then(time),
        duration: get("DURATION").and_then(|p| duration(&p.value)),
        rrule: get("RRULE").map(|p| p.value.clone()),
        exdates: props.iter().filter(|p| p.name == "EXDATE").flat_map(times).collect(),
        recurrence_id: get("RECURRENCE-ID").and_then(time),
    })
}

/// The time of a property such as `DTSTART;TZID=Europe/Berlin:20261029T090000`.
fn time(property: &Property) -> Option<Time> {
    parse_time(&property.value, property.param("VALUE") == Some("DATE"), property.param("TZID"))
}

/// All times of a list property such as `EXDATE`.
fn times(property: &Property) -> Vec<Time> {
    let is_date = property.param("VALUE") == Some("DATE");
    property.value.split(',').filter_map(|value| parse_time(value, is_date, property.param("TZID"))).collect()
}

fn parse_time(value: &str, is_date: bool, tz: Option<&str>) -> Option<Time> {
    let value = value.trim();
    if is_date || value.len() == 8 {
        return NaiveDate::parse_from_str(value, "%Y%m%d").ok().map(|date| Time::Date { date });
    }
    let (local, utc) = match value.strip_suffix('Z') {
        Some(local) => (local, true),
        None => (value, false),
    };
    let at = NaiveDateTime::parse_from_str(local, "%Y%m%dT%H%M%S").ok()?;
    Some(match (utc, tz) {
        (true, _) => Time::Utc { at: at.and_utc() },
        (false, Some(tz)) => Time::Zoned { at, tz: tz.to_owned() },
        (false, None) => Time::Floating { at },
    })
}

/// Seconds of an iCalendar duration such as `PT1H30M` or `-P1D`.
pub fn duration(value: &str) -> Option<i64> {
    let (sign, rest) = match value.trim().strip_prefix('-') {
        Some(rest) => (-1, rest),
        None => (1, value.trim().trim_start_matches('+')),
    };
    let mut rest = rest.strip_prefix('P')?;
    let mut total = 0i64;
    let mut in_time = false;
    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix('T') {
            in_time = true;
            rest = after;
            continue;
        }
        let digits = rest.find(|c: char| !c.is_ascii_digit())?;
        let number: i64 = rest[..digits].parse().ok()?;
        let unit = match (rest[digits..].chars().next()?, in_time) {
            ('W', false) => 7 * 86_400,
            ('D', false) => 86_400,
            ('H', true) => 3_600,
            ('M', true) => 60,
            ('S', true) => 1,
            _ => return None,
        };
        total += number * unit;
        rest = &rest[digits + 1..];
    }
    Some(sign * total)
}

/// Text values escape `\`, `;`, `,` and new lines.
pub fn unescape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        match (c, c == '\\') {
            (_, true) => match chars.next() {
                Some('n' | 'N') => out.push('\n'),
                Some(other) => out.push(other),
                None => {}
            },
            (c, false) => out.push(c),
        }
    }
    out
}

pub fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.replace("\r\n", "\n").chars() {
        match c {
            '\\' | ';' | ',' => {
                out.push('\\');
                out.push(c);
            }
            '\n' => out.push_str("\\n"),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

/// `NAME;…:value` of a time property.
pub fn time_line(name: &str, time: &Time) -> String {
    match time {
        Time::Date { date } => format!("{name};VALUE=DATE:{}", date.format("%Y%m%d")),
        Time::Utc { at } => format!("{name}:{}", at.format("%Y%m%dT%H%M%SZ")),
        Time::Zoned { at, tz } => format!("{name};TZID={tz}:{}", at.format("%Y%m%dT%H%M%S")),
        Time::Floating { at } => format!("{name}:{}", at.format("%Y%m%dT%H%M%S")),
    }
}

/// What the web page edits in an event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fields {
    pub summary: String,
    pub location: String,
    pub description: String,
    pub start: Time,
    pub end: Time,
    pub repeat: Repeat,
}

/// What happens to the event's `RRULE`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Repeat {
    /// Leave it as it is (a rule the page can't show, made by another client).
    Keep,
    Never,
    Rule(String),
}

/// A new file with one event.
pub fn new_file(uid: &str, fields: &Fields, now: DateTime<Utc>) -> String {
    let mut lines = vec![
        "BEGIN:VCALENDAR".to_owned(),
        "VERSION:2.0".to_owned(),
        "PRODID:-//RustCloud//Calendar//EN".to_owned(),
        "BEGIN:VEVENT".to_owned(),
        format!("UID:{uid}"),
    ];
    lines.extend(field_lines(fields, now, 0));
    lines.extend(["END:VEVENT".to_owned(), "END:VCALENDAR".to_owned()]);
    fold(&lines)
}

/// `text` with the fields of its main event (not a changed occurrence)
/// replaced; every other line stays.
pub fn update_file(text: &str, fields: &Fields, now: DateTime<Utc>) -> Option<String> {
    let lines = unfold(text);
    let mut replaced = vec!["SUMMARY", "LOCATION", "DESCRIPTION", "DTSTART", "DTEND", "DURATION"];
    replaced.extend(["DTSTAMP", "LAST-MODIFIED", "SEQUENCE"]);
    if fields.repeat != Repeat::Keep {
        replaced.extend(["RRULE", "EXDATE"]);
    }

    // Find the main event: the VEVENT without RECURRENCE-ID.
    let mut out = Vec::with_capacity(lines.len() + 4);
    let mut i = 0;
    let mut done = false;
    while i < lines.len() {
        let is_event =
            parse_line(&lines[i]).is_some_and(|p| p.name == "BEGIN" && p.value.eq_ignore_ascii_case("VEVENT"));
        if !is_event || done {
            out.push(lines[i].clone());
            i += 1;
            continue;
        }
        let end = (i..lines.len()).find(|&j| {
            parse_line(&lines[j]).is_some_and(|p| p.name == "END" && p.value.eq_ignore_ascii_case("VEVENT"))
        })?;
        let body = &lines[i + 1..end];
        let props: Vec<Option<Property>> = body.iter().map(|line| parse_line(line)).collect();
        if props.iter().flatten().any(|p| p.name == "RECURRENCE-ID") {
            out.extend_from_slice(&lines[i..=end]);
            i = end + 1;
            continue;
        }
        let sequence = props
            .iter()
            .flatten()
            .find(|p| p.name == "SEQUENCE")
            .and_then(|p| p.value.trim().parse::<u32>().ok())
            .map_or(0, |n| n + 1);
        out.push(lines[i].clone());
        out.extend(field_lines(fields, now, sequence));
        let mut nested = 0;
        for (line, prop) in body.iter().zip(&props) {
            let name = prop.as_ref().map_or("", |p| p.name.as_str());
            match name {
                "BEGIN" => nested += 1,
                "END" => nested -= 1,
                _ if nested == 0 && replaced.contains(&name) => continue,
                _ => {}
            }
            out.push(line.clone());
        }
        out.push(lines[end].clone());
        i = end + 1;
        done = true;
    }
    done.then(|| fold(&out))
}

fn field_lines(fields: &Fields, now: DateTime<Utc>, sequence: u32) -> Vec<String> {
    let stamp = now.format("%Y%m%dT%H%M%SZ");
    let mut lines = vec![
        format!("DTSTAMP:{stamp}"),
        format!("LAST-MODIFIED:{stamp}"),
        format!("SEQUENCE:{sequence}"),
        time_line("DTSTART", &fields.start),
        time_line("DTEND", &fields.end),
        format!("SUMMARY:{}", escape(&fields.summary)),
    ];
    if !fields.location.is_empty() {
        lines.push(format!("LOCATION:{}", escape(&fields.location)));
    }
    if !fields.description.is_empty() {
        lines.push(format!("DESCRIPTION:{}", escape(&fields.description)));
    }
    if let Repeat::Rule(rule) = &fields.repeat {
        lines.push(format!("RRULE:{rule}"));
    }
    lines
}

/// Lines joined with CRLF, each folded at 75 bytes.
fn fold(lines: &[String]) -> String {
    let mut out = String::new();
    for line in lines {
        let mut start = 0;
        let mut limit = 75;
        while line.len() - start > limit {
            let mut end = start + limit;
            while !line.is_char_boundary(end) {
                end -= 1;
            }
            out.push_str(&line[start..end]);
            out.push_str("\r\n ");
            start = end;
            // The leading space of a continuation counts too.
            limit = 74;
        }
        out.push_str(&line[start..]);
        out.push_str("\r\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const FROM_CLIENT: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Other//EN\r\nBEGIN:VEVENT\r\n\
        UID:abc@example.com\r\nDTSTART;TZID=Europe/Berlin:20261029T090000\r\nDURATION:PT1H30M\r\n\
        SUMMARY:Stand-up\\, daily\r\nDESCRIPTION:Line one\\nline t\r\n wo\r\nRRULE:FREQ=WEEKLY;BYDAY=MO,WE\r\n\
        EXDATE;TZID=Europe/Berlin:20261102T090000,20261104T090000\r\nSEQUENCE:3\r\nBEGIN:VALARM\r\n\
        ACTION:DISPLAY\r\nDESCRIPTION:Reminder\r\nTRIGGER:-PT10M\r\nEND:VALARM\r\nEND:VEVENT\r\n\
        BEGIN:VEVENT\r\nUID:abc@example.com\r\nRECURRENCE-ID;TZID=Europe/Berlin:20261109T090000\r\n\
        DTSTART;TZID=Europe/Berlin:20261109T100000\r\nDTEND;TZID=Europe/Berlin:20261109T110000\r\n\
        SUMMARY:Moved\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

    fn at(text: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M").unwrap()
    }

    #[test]
    fn events_are_read_with_folding_escapes_and_alarms_skipped() {
        let events = events(FROM_CLIENT);
        assert_eq!(events.len(), 2);
        let main = &events[0];
        assert_eq!(main.uid, "abc@example.com");
        assert_eq!(main.summary, "Stand-up, daily");
        assert_eq!(main.description, "Line one\nline two", "the alarm's description is not the event's");
        assert_eq!(main.start, Time::Zoned { at: at("2026-10-29 09:00"), tz: "Europe/Berlin".into() });
        assert_eq!(main.duration, Some(5400));
        assert_eq!(main.rrule.as_deref(), Some("FREQ=WEEKLY;BYDAY=MO,WE"));
        assert_eq!(main.exdates.len(), 2);
        assert!(events[1].recurrence_id.is_some());
    }

    #[test]
    fn times_and_durations() {
        let all_day = parse_line("DTSTART;VALUE=DATE:20261231").unwrap();
        assert_eq!(time(&all_day), Some(Time::Date { date: NaiveDate::from_ymd_opt(2026, 12, 31).unwrap() }));
        let utc = parse_line("DTSTART:20261029T090000Z").unwrap();
        assert_eq!(time(&utc), Some(Time::Utc { at: at("2026-10-29 09:00").and_utc() }));
        let floating = parse_line("DTSTART:20261029T090000").unwrap();
        assert_eq!(time(&floating), Some(Time::Floating { at: at("2026-10-29 09:00") }));
        assert_eq!(duration("P1W2D"), Some(9 * 86_400));
        assert_eq!(duration("-PT15M"), Some(-900));
        assert_eq!(duration("1H"), None);
    }

    #[test]
    fn quoted_parameters_may_hold_colons() {
        let line = parse_line("ATTENDEE;CN=\"Doe: John\";ROLE=CHAIR:mailto:john@example.com").unwrap();
        assert_eq!(line.param("CN"), Some("Doe: John"));
        assert_eq!(line.value, "mailto:john@example.com");
    }

    fn fields(repeat: Repeat) -> Fields {
        Fields {
            summary: "Lunch; with, friends".into(),
            location: String::new(),
            description: "Bring\ncake".into(),
            start: Time::Zoned { at: at("2026-10-30 12:00"), tz: "Europe/Berlin".into() },
            end: Time::Zoned { at: at("2026-10-30 13:00"), tz: "Europe/Berlin".into() },
            repeat,
        }
    }

    #[test]
    fn a_new_file_reads_back() {
        let now = at("2026-10-29 10:00").and_utc();
        let text = new_file("id-1", &fields(Repeat::Rule("FREQ=DAILY".into())), now);
        assert!(text.ends_with("END:VCALENDAR\r\n"));
        let event = &events(&text)[0];
        assert_eq!(event.uid, "id-1");
        assert_eq!(event.summary, "Lunch; with, friends");
        assert_eq!(event.description, "Bring\ncake");
        assert_eq!(event.rrule.as_deref(), Some("FREQ=DAILY"));
    }

    #[test]
    fn updates_keep_what_the_page_does_not_edit() {
        let now = at("2026-10-29 10:00").and_utc();
        let text = update_file(FROM_CLIENT, &fields(Repeat::Keep), now).unwrap();
        let events = events(&text);
        assert_eq!(events[0].summary, "Lunch; with, friends");
        assert_eq!(events[0].duration, None, "DTEND replaces DURATION");
        assert_eq!(events[0].rrule.as_deref(), Some("FREQ=WEEKLY;BYDAY=MO,WE"), "kept");
        assert_eq!(events[0].exdates.len(), 2);
        assert!(text.contains("TRIGGER:-PT10M"), "the alarm stays");
        assert!(text.contains("SEQUENCE:4\r\n"));
        assert_eq!(events[1].summary, "Moved", "the changed occurrence stays");

        let text = update_file(FROM_CLIENT, &fields(Repeat::Never), now).unwrap();
        assert_eq!(events_rrule(&text), None);
        assert!(!text.contains("EXDATE"));
    }

    fn events_rrule(text: &str) -> Option<String> {
        events(text)[0].rrule.clone()
    }

    #[test]
    fn long_lines_are_folded_at_75_bytes() {
        let mut long = fields(Repeat::Never);
        long.description = "ж".repeat(100);
        let text = new_file("id", &long, Utc::now());
        assert!(text.split("\r\n").all(|line| line.len() <= 75));
        assert_eq!(events(&text)[0].description, long.description);
    }
}
