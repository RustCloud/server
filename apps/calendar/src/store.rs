//! Calendars in the app's AppData:
//!
//! ```text
//! calendars/<id>/calendar.json   name and colour
//! calendars/<id>/<name>.ics      one file per event (with its changed occurrences)
//! index/<id>.json                what the files say, refreshed by etag
//! ```
//!
//! CalDAV clients pick the file names; the web page uses the event's UID.
//!
//! Calendars other users shared are folders of their AppData, reached
//! through the `shares` import; their id here is `shared-<share id>`, and
//! their index lies in this user's AppData like any other.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::ical::{self, Event};
use crate::rustcloud::app::appdata::{self, Entry, Error};
use crate::rustcloud::app::shares;

const CALENDARS: &str = "calendars";
const INFO_FILE: &str = "calendar.json";
const DEFAULT_ID: &str = "personal";
/// Ids of calendars shared by other users start with this.
const SHARED: &str = "shared-";
/// Colour of new calendars.
pub const DEFAULT_COLOR: &str = "#3b82f6";
/// The message of [`Error::Other`] when a calendar is shared read-only.
pub const READ_ONLY: &str = "read-only";
/// How much of a description the index keeps.
const INDEX_DESCRIPTION: usize = 500;

#[derive(Debug, Clone, Serialize)]
pub struct Calendar {
    pub id: String,
    pub name: String,
    pub color: String,
    /// Who shared it; none for the user's own calendars.
    pub owner: Option<Owner>,
    /// Whether events can be added and changed.
    pub writable: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Owner {
    pub username: String,
    pub display_name: String,
}

/// What `calendar.json` holds.
#[derive(Serialize, Deserialize)]
struct Info {
    name: String,
    color: String,
}

impl Calendar {
    /// A new calendar of the user's own.
    pub fn own(id: String, name: String, color: String) -> Self {
        Self { id, name, color, owner: None, writable: true }
    }
}

/// The user's calendars, by name, then those shared with the user. The
/// first visit creates "Calendar".
pub fn calendars() -> Result<Vec<Calendar>, Error> {
    let mut found = Vec::new();
    for entry in list_or_empty(CALENDARS)?.into_iter().filter(|e| e.is_dir) {
        if let Some(calendar) = calendar(&entry.name)? {
            found.push(calendar);
        }
    }
    if found.is_empty() {
        let personal = Calendar::own(DEFAULT_ID.into(), "Calendar".into(), DEFAULT_COLOR.into());
        // Can't be saved with a read-only app password; it is saved on the
        // first change then.
        let _ = save_calendar(&personal);
        found.push(personal);
    }
    for share in received()? {
        if let Some(calendar) = shared_calendar(&share)? {
            found.push(calendar);
        }
    }
    found.sort_by_key(|calendar| (calendar.owner.is_some(), calendar.name.to_lowercase()));
    Ok(found)
}

pub fn calendar(id: &str) -> Result<Option<Calendar>, Error> {
    if !valid_id(id) {
        return Ok(None);
    }
    if let Some(share_id) = id.strip_prefix(SHARED) {
        return match received()?.iter().find(|share| share.id == share_id) {
            Some(share) => shared_calendar(share),
            None => Ok(None),
        };
    }
    Ok(read_info(id)?.map(|info| Calendar::own(id.to_owned(), info.name, info.color)))
}

fn shared_calendar(share: &shares::Share) -> Result<Option<Calendar>, Error> {
    let id = format!("{SHARED}{}", share.id);
    Ok(read_info(&id)?.map(|info| Calendar {
        id,
        name: info.name,
        color: info.color,
        owner: Some(Owner { username: share.owner.username.clone(), display_name: share.owner.display_name.clone() }),
        writable: share.write,
    }))
}

/// Shared calendars: folders of other users' calendars, not anything else
/// of their AppData.
fn received() -> Result<Vec<shares::Share>, Error> {
    let all = shares::received().map_err(shared_error)?;
    Ok(all.into_iter().filter(|share| share.path.starts_with(&format!("{CALENDARS}/"))).collect())
}

fn read_info(id: &str) -> Result<Option<Info>, Error> {
    match read(id, INFO_FILE) {
        Ok(data) => Ok(Some(
            serde_json::from_slice(&data).unwrap_or_else(|_| Info { name: id.into(), color: DEFAULT_COLOR.into() }),
        )),
        Err(Error::NotFound) => Ok(None),
        Err(err) => Err(err),
    }
}

pub fn save_calendar(calendar: &Calendar) -> Result<(), Error> {
    let info = Info { name: calendar.name.clone(), color: calendar.color.clone() };
    let data = serde_json::to_vec(&info).map_err(|err| Error::Other(err.to_string()))?;
    write(&calendar.id, INFO_FILE, &data).map(drop)
}

/// Deletes one of the user's calendars; a shared one is only given up.
pub fn delete_calendar(id: &str) -> Result<(), Error> {
    match id.strip_prefix(SHARED) {
        Some(share_id) => shares::leave(share_id).map_err(shared_error)?,
        None => appdata::delete(&format!("{CALENDARS}/{id}"))?,
    }
    ignore_missing(appdata::delete(&index_path(id)))
}

/// Whether `id` can name a calendar: it is a folder name and part of URLs.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 100
        && !id.starts_with('.')
        && id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// Whether `id` names a calendar another user shared.
pub fn is_shared(id: &str) -> bool {
    id.starts_with(SHARED)
}

/// A new calendar id, unique for this user.
pub fn new_id() -> String {
    crate::random_id()
}

/// The `.ics` files of a calendar.
pub fn objects(calendar: &str) -> Result<Vec<Entry>, Error> {
    let entries = match calendar.strip_prefix(SHARED) {
        Some(share_id) => shares::list(share_id, "").map_err(shared_error)?,
        None => list_or_empty(&format!("{CALENDARS}/{calendar}"))?,
    };
    Ok(entries.into_iter().filter(|e| !e.is_dir && is_object(&e.name)).collect())
}

pub fn object(calendar: &str, name: &str) -> Result<Option<Entry>, Error> {
    Ok(objects(calendar)?.into_iter().find(|e| e.name == name))
}

pub fn read_object(calendar: &str, name: &str) -> Result<Vec<u8>, Error> {
    read(calendar, name)
}

pub fn write_object(calendar: &str, name: &str, data: &[u8]) -> Result<Entry, Error> {
    write(calendar, name, data)
}

pub fn delete_object(calendar: &str, name: &str) -> Result<(), Error> {
    match calendar.strip_prefix(SHARED) {
        Some(share_id) => shares::delete(share_id, name).map_err(shared_error),
        None => appdata::delete(&format!("{CALENDARS}/{calendar}/{name}")),
    }
}

/// A file of a calendar's folder, the user's own or shared.
fn read(calendar: &str, file: &str) -> Result<Vec<u8>, Error> {
    match calendar.strip_prefix(SHARED) {
        Some(share_id) => shares::read(share_id, file).map_err(shared_error),
        None => appdata::read(&format!("{CALENDARS}/{calendar}/{file}")),
    }
}

fn write(calendar: &str, file: &str, data: &[u8]) -> Result<Entry, Error> {
    match calendar.strip_prefix(SHARED) {
        Some(share_id) => shares::write(share_id, file, data).map_err(shared_error),
        None => appdata::write(&format!("{CALENDARS}/{calendar}/{file}"), data),
    }
}

/// The same errors as for AppData; a read-only share is [`READ_ONLY`].
fn shared_error(err: shares::Error) -> Error {
    match err {
        shares::Error::NoUser => Error::NoUser,
        shares::Error::NotFound => Error::NotFound,
        shares::Error::ReadOnly => Error::Other(READ_ONLY.into()),
        shares::Error::InvalidPath => Error::InvalidPath,
        shares::Error::NoSpace => Error::NoSpace,
        shares::Error::TooLarge => Error::TooLarge,
        shares::Error::Conflict => Error::Conflict,
        shares::Error::Other(message) => Error::Other(message),
    }
}

/// Whether a file of a calendar's folder is an event (not `calendar.json`).
pub fn is_object(name: &str) -> bool {
    name.to_ascii_lowercase().ends_with(".ics") && !name.starts_with('.')
}

/// A fingerprint of a calendar's contents (CalDAV `getctag`): changes when
/// any file is added, changed or removed.
pub fn tag(objects: &[Entry]) -> String {
    // FNV-1a: small and stable; this is not about security.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in objects.iter().flat_map(|e| e.name.bytes().chain([0]).chain(e.etag.bytes()).chain([0])) {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}-{}", objects.len())
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Index {
    files: BTreeMap<String, Indexed>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Indexed {
    etag: String,
    events: Vec<Event>,
}

/// The events of a calendar by file name, read from the index; files whose
/// etag changed since are read again.
pub fn events(calendar: &str) -> Result<Vec<(String, Vec<Event>)>, Error> {
    let path = index_path(calendar);
    let mut index: Index =
        appdata::read(&path).ok().and_then(|data| serde_json::from_slice(&data).ok()).unwrap_or_default();
    let objects = objects(calendar)?;
    let mut changed = index.files.len() != objects.len();
    let mut fresh = BTreeMap::new();
    for entry in objects {
        let indexed = match index.files.remove(&entry.name) {
            Some(indexed) if indexed.etag == entry.etag => indexed,
            _ => {
                changed = true;
                let text = String::from_utf8_lossy(&read_object(calendar, &entry.name)?).into_owned();
                let mut events = ical::events(&text);
                for event in &mut events {
                    event.description = cut(&event.description, INDEX_DESCRIPTION);
                }
                Indexed { etag: entry.etag, events }
            }
        };
        fresh.insert(entry.name, indexed);
    }
    let index = Index { files: fresh };
    if changed {
        // If saving fails (a read-only app password), the next look rebuilds it.
        if let Ok(data) = serde_json::to_vec(&index) {
            let _ = appdata::write(&path, &data);
        }
    }
    Ok(index.files.into_iter().map(|(name, indexed)| (name, indexed.events)).collect())
}

fn index_path(calendar: &str) -> String {
    format!("index/{calendar}.json")
}

fn list_or_empty(dir: &str) -> Result<Vec<Entry>, Error> {
    match appdata::list(dir) {
        Err(Error::NotFound) => Ok(Vec::new()),
        other => other,
    }
}

fn ignore_missing(result: Result<(), Error>) -> Result<(), Error> {
    match result {
        Err(Error::NotFound) => Ok(()),
        other => other,
    }
}

/// At most `max` bytes of `text`, cut at a character boundary.
fn cut(text: &str, max: usize) -> String {
    let mut end = text.len().min(max);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}
