//! The JSON API of the calendar's page (reached through the panel's bridge).
//!
//! | request                              | what it does                                   |
//! |--------------------------------------|------------------------------------------------|
//! | `GET /api/calendars`                 | the calendars                                  |
//! | `POST /api/calendars`                | a new calendar `{name, color}`                 |
//! | `PUT /api/calendars/<id>`            | rename, recolour `{name, color}`               |
//! | `DELETE /api/calendars/<id>`         | delete with its events; a shared one: give up  |
//! | `GET /api/events?from=&to=`          | occurrences in a range (RFC 3339)              |
//! | `GET /api/events/<cal>/<name>?tz=`   | an event to edit, times in the zone `tz`       |
//! | `POST /api/events/<cal>`             | a new event (`Form`)                           |
//! | `PUT /api/events/<cal>/<name>`       | save (`Form` with `etag`); 409 if changed      |
//! | `DELETE /api/events/<cal>/<name>`    | delete an event with all its repeats           |
//! | `GET /api/connect`                   | what a CalDAV client needs to connect          |

use chrono::{DateTime, Duration, NaiveDate, NaiveDateTime, Utc};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::expand::{self, instant};
use crate::ical::{self, Event, Fields, Repeat, Time};
use crate::rustcloud::app::appdata::Error;
use crate::store::{self, Calendar};
use crate::{dav, query_param, Response, User};

/// Longest range of one `GET /api/events`.
const MAX_RANGE_DAYS: i64 = 400;
const REPEATS: [(&str, &str); 4] =
    [("daily", "DAILY"), ("weekly", "WEEKLY"), ("monthly", "MONTHLY"), ("yearly", "YEARLY")];

pub fn handle(user: &User, method: &str, path: &str, query: &str, body: &[u8]) -> Response {
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let result = match (method, segments.as_slice()) {
        ("GET", ["calendars"]) => store::calendars().map(|list| Ok(json!(list))),
        ("POST", ["calendars"]) => parse::<CalendarForm>(body).map(create_calendar),
        ("PUT", ["calendars", id]) => parse::<CalendarForm>(body).map(|form| update_calendar(id, form)),
        ("DELETE", ["calendars", id]) => delete_calendar(id),
        ("GET", ["events"]) => events(query),
        ("GET", ["events", calendar, name]) => event(calendar, name, &query_param(query, "tz")),
        ("POST", ["events", calendar]) => parse::<Form>(body).map(|form| create_event(calendar, form)),
        ("PUT", ["events", calendar, name]) => parse::<Form>(body).map(|form| update_event(calendar, name, form)),
        ("DELETE", ["events", calendar, name]) => delete_event(calendar, name),
        ("GET", ["connect"]) => Ok(Ok(json!({ "path": format!("{}/", dav::BASE), "username": user.username }))),
        _ => Ok(Err(failure(404, "not found"))),
    };
    match result {
        Ok(Ok(value)) => ok(value),
        Ok(Err(response)) => response,
        Err(err) => failure(storage_status(&err), &format!("{err:?}")),
    }
}

/// What a handler answers: storage errors become an error response in
/// `handle`; `Err(response)` inside is an answer of its own (400, 409…).
type Answer = Result<Result<Value, Response>, Error>;

fn parse<T: for<'de> Deserialize<'de>>(body: &[u8]) -> Result<T, Error> {
    serde_json::from_slice(body).map_err(|err| Error::Other(format!("invalid request: {err}")))
}

#[derive(Deserialize)]
struct CalendarForm {
    name: String,
    #[serde(default)]
    color: String,
}

fn create_calendar(form: CalendarForm) -> Result<Value, Response> {
    let calendar = calendar_from(store::new_id(), form)?;
    store::save_calendar(&calendar).map_err(storage_failure)?;
    Ok(json!(calendar))
}

fn update_calendar(id: &str, form: CalendarForm) -> Result<Value, Response> {
    match store::calendar(id).map_err(storage_failure)? {
        None => return Err(failure(404, "no such calendar")),
        Some(calendar) if calendar.owner.is_some() => {
            return Err(failure(403, "only the owner renames a shared calendar"))
        }
        Some(_) => {}
    }
    let calendar = calendar_from(id.to_owned(), form)?;
    store::save_calendar(&calendar).map_err(storage_failure)?;
    Ok(json!(calendar))
}

fn calendar_from(id: String, form: CalendarForm) -> Result<Calendar, Response> {
    let name = form.name.trim();
    if name.is_empty() || name.chars().count() > 100 {
        return Err(failure(400, "a calendar needs a name of up to 100 characters"));
    }
    let color =
        if form.color.starts_with('#') && form.color.len() <= 9 { form.color } else { store::DEFAULT_COLOR.into() };
    Ok(Calendar::own(id, name.to_owned(), color))
}

fn delete_calendar(id: &str) -> Answer {
    let own = store::calendars()?.iter().filter(|calendar| calendar.owner.is_none()).count();
    if !store::is_shared(id) && own <= 1 {
        return Ok(Err(failure(409, "the last calendar can't be deleted")));
    }
    store::delete_calendar(id)?;
    Ok(Ok(json!({})))
}

/// Occurrences of all events in `[from, to)`.
fn events(query: &str) -> Answer {
    let range = |name| DateTime::parse_from_rfc3339(&query_param(query, name)).ok().map(|at| at.with_timezone(&Utc));
    let (Some(from), Some(to)) = (range("from"), range("to")) else {
        return Ok(Err(failure(400, "from and to must be RFC 3339 times")));
    };
    if to <= from || to - from > Duration::days(MAX_RANGE_DAYS) {
        return Ok(Err(failure(400, "the range must be positive and at most 400 days")));
    }
    let mut found = Vec::new();
    for calendar in store::calendars()? {
        for (name, events) in store::events(&calendar.id)? {
            let moved: Vec<DateTime<Utc>> =
                events.iter().filter_map(|e| e.recurrence_id.as_ref()).map(instant).collect();
            for event in &events {
                // Changed occurrences are events of their own; the main one skips them.
                let skip: &[DateTime<Utc>] = if event.recurrence_id.is_none() { &moved } else { &[] };
                for occurrence in expand::occurrences(event, skip, from, to) {
                    found.push(json!({
                        "calendar": calendar.id,
                        "name": name,
                        "summary": event.summary,
                        "location": event.location,
                        "start": occurrence.start,
                        "end": occurrence.end,
                        "all_day": occurrence.all_day,
                        "recurring": event.rrule.is_some() || event.recurrence_id.is_some(),
                    }));
                }
            }
        }
    }
    Ok(Ok(json!(found)))
}

/// An event as the edit form shows it, times in the viewer's zone `tz`.
fn event(calendar: &str, name: &str, tz: &str) -> Answer {
    let Some(entry) = store::object(calendar, name)? else {
        return Ok(Err(failure(404, "no such event")));
    };
    let text = String::from_utf8_lossy(&store::read_object(calendar, name)?).into_owned();
    let Some(event) = main_event(&text) else {
        return Ok(Err(failure(422, "the file has no event")));
    };
    let viewer = expand::time_zone(tz);
    let show = |time: &Time| match (time, viewer) {
        (Time::Date { date }, _) => date.format("%Y-%m-%d").to_string(),
        (Time::Floating { at }, _) => at.format("%Y-%m-%dT%H:%M").to_string(),
        (_, Some(zone)) => instant(time).with_timezone(&zone).format("%Y-%m-%dT%H:%M").to_string(),
        (_, None) => instant(time).format("%Y-%m-%dT%H:%M").to_string(),
    };
    let end = event.end.clone().unwrap_or_else(|| shifted(&event.start, expand::length(&event)));
    let repeat = match event.rrule.as_deref() {
        None => "none",
        Some(rule) => {
            REPEATS.iter().find(|(_, freq)| rule == format!("FREQ={freq}")).map_or("custom", |(name, _)| *name)
        }
    };
    Ok(Ok(json!({
        "calendar": calendar,
        "name": name,
        "summary": event.summary,
        "location": event.location,
        "description": event.description,
        "all_day": matches!(event.start, Time::Date { .. }),
        "start": show(&event.start),
        "end": show(&end),
        "repeat": repeat,
        "rule": event.rrule,
        "etag": entry.etag,
        "writable": writable(calendar)?,
    })))
}

/// The event that is not a changed occurrence.
fn main_event(text: &str) -> Option<Event> {
    let events = ical::events(text);
    events.iter().find(|e| e.recurrence_id.is_none()).or(events.first()).cloned()
}

/// `time` moved by `by`, keeping its kind.
fn shifted(time: &Time, by: Duration) -> Time {
    match time {
        Time::Date { date } => Time::Date { date: *date + Duration::days(by.num_days().max(1)) },
        Time::Utc { at } => Time::Utc { at: *at + by },
        Time::Zoned { at, tz } => Time::Zoned { at: *at + by, tz: tz.clone() },
        Time::Floating { at } => Time::Floating { at: *at + by },
    }
}

/// An event as the page sends it.
#[derive(Deserialize)]
struct Form {
    summary: String,
    #[serde(default)]
    location: String,
    #[serde(default)]
    description: String,
    all_day: bool,
    /// `2026-10-29` for all-day events (the end is the day after the last),
    /// else `2026-10-29T09:00`: wall-clock time in `tz`.
    start: String,
    end: String,
    /// The browser's time zone, e.g. `Europe/Berlin`.
    #[serde(default)]
    tz: String,
    /// `none`, `daily`, `weekly`, `monthly`, `yearly`; `custom` keeps the rule.
    #[serde(default)]
    repeat: String,
    /// Of the file as the page opened it; saving refuses if it changed since.
    #[serde(default)]
    etag: String,
}

impl Form {
    fn fields(&self) -> Result<Fields, Response> {
        let time = |text: &str| -> Option<Time> {
            if self.all_day {
                return NaiveDate::parse_from_str(text, "%Y-%m-%d").ok().map(|date| Time::Date { date });
            }
            let at = NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%M")
                .or_else(|_| NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%M:%S"))
                .ok()?;
            Some(match expand::time_zone(&self.tz) {
                Some(zone) => Time::Zoned { at, tz: zone.name().to_owned() },
                None => Time::Floating { at },
            })
        };
        let (Some(start), Some(end)) = (time(&self.start), time(&self.end)) else {
            return Err(failure(400, "start and end must be dates or times"));
        };
        if instant(&end) < instant(&start) || (self.all_day && end == start) {
            return Err(failure(400, "the event must end after it starts"));
        }
        let repeat = match self.repeat.as_str() {
            "" | "none" => Repeat::Never,
            "custom" => Repeat::Keep,
            other => match REPEATS.iter().find(|(name, _)| *name == other) {
                Some((_, freq)) => Repeat::Rule(format!("FREQ={freq}")),
                None => return Err(failure(400, "unknown repeat")),
            },
        };
        let summary = self.summary.trim();
        Ok(Fields {
            summary: if summary.is_empty() { "New event".into() } else { summary.to_owned() },
            location: self.location.trim().to_owned(),
            description: self.description.trim().to_owned(),
            start,
            end,
            repeat,
        })
    }
}

fn create_event(calendar: &str, form: Form) -> Result<Value, Response> {
    match store::calendar(calendar).map_err(storage_failure)? {
        None => return Err(failure(404, "no such calendar")),
        Some(found) if !found.writable => return Err(read_only()),
        Some(_) => {}
    }
    let mut fields = form.fields()?;
    if fields.repeat == Repeat::Keep {
        fields.repeat = Repeat::Never;
    }
    let uid = crate::random_id();
    let text = ical::new_file(&uid, &fields, Utc::now());
    let name = format!("{uid}.ics");
    let entry = store::write_object(calendar, &name, text.as_bytes()).map_err(storage_failure)?;
    Ok(json!({ "calendar": calendar, "name": name, "etag": entry.etag }))
}

fn update_event(calendar: &str, name: &str, form: Form) -> Result<Value, Response> {
    if !writable(calendar).map_err(storage_failure)? {
        return Err(read_only());
    }
    let fields = form.fields()?;
    let current =
        store::object(calendar, name).map_err(storage_failure)?.ok_or_else(|| failure(404, "no such event"))?;
    if current.etag != form.etag {
        return Err(json_response(409, json!({ "error": "changed elsewhere", "etag": current.etag })));
    }
    let text = String::from_utf8_lossy(&store::read_object(calendar, name).map_err(storage_failure)?).into_owned();
    let updated = ical::update_file(&text, &fields, Utc::now()).ok_or_else(|| failure(422, "the file has no event"))?;
    let entry = store::write_object(calendar, name, updated.as_bytes()).map_err(storage_failure)?;
    Ok(json!({ "calendar": calendar, "name": name, "etag": entry.etag }))
}

fn delete_event(calendar: &str, name: &str) -> Answer {
    if !writable(calendar)? {
        return Ok(Err(read_only()));
    }
    store::delete_object(calendar, name)?;
    Ok(Ok(json!({})))
}

/// Whether events of the calendar can be changed (it may be shared read-only).
fn writable(calendar: &str) -> Result<bool, Error> {
    Ok(store::calendar(calendar)?.is_some_and(|calendar| calendar.writable))
}

fn read_only() -> Response {
    failure(403, "this calendar is shared with you read-only")
}

fn storage_status(err: &Error) -> u16 {
    match err {
        Error::NoUser => 401,
        Error::NotFound => 404,
        Error::InvalidPath => 400,
        Error::NoSpace | Error::TooLarge => 413,
        Error::Conflict => 409,
        Error::Other(message) if message.starts_with("invalid request") => 400,
        Error::Other(message) if message == store::READ_ONLY => 403,
        Error::Other(_) => 500,
    }
}

fn storage_failure(err: Error) -> Response {
    failure(storage_status(&err), &format!("{err:?}"))
}

fn ok(value: Value) -> Response {
    json_response(200, value)
}

fn failure(status: u16, message: &str) -> Response {
    json_response(status, json!({ "error": message }))
}

fn json_response(status: u16, value: Value) -> Response {
    Response {
        status,
        headers: vec![("content-type".to_owned(), "application/json".to_owned())],
        body: value.to_string().into_bytes(),
    }
}
