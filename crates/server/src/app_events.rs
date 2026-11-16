//! What the panel hears of plugins for its user, over one connection per tab
//! (`GET /api/v1/apps/events`, server-sent events):
//!
//! - `changed` `{"app"}` — a job another user added for this user (a chat
//!   message) is done: the plugin's page, if open, reads its AppData again;
//! - `badge` `{"app","count"}` — the number by the plugin in the side panel
//!   (`notify.badge`); all of them when the connection opens;
//! - `ring` `{"id","app","from":{"id","username"},"kind","ref","until"}` — a
//!   call or message over any page (`notify.ring`); the rings still on when
//!   the connection opens; `ring-end` `{"id"}` when it is over: cancelled,
//!   dismissed, timed out.
//!
//! No event carries what a plugin keeps: only which plugin and a number, or
//! who rings and why. The text of a ring is the panel's, in the user's
//! language, with the plugin's name and the sender's username, so a plugin
//! can't pass a ring off as a message of the cloud.

use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::{Path, State};
use axum::http::{header, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::IntoResponse;
use axum::routing::{delete, get};
use axum::Router;
use chrono::{DateTime, Utc};
use rustcloud_auth::CurrentUser;
use rustcloud_core::{Error, ErrorKind, Result};
use rustcloud_db::{plugin_badge, DatabaseConnection};
use sea_orm::sea_query::OnConflict;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, Set};
use serde::Serialize;
use tokio::sync::broadcast;
use tokio::sync::broadcast::error::RecvError;
use uuid::Uuid;

use crate::AppState;

/// Connections one user may hold at once.
const MAX_CONNECTIONS: usize = 10;
/// A connection ends after this; the panel opens a new one.
const CONNECTION_TIME: Duration = Duration::from_secs(10 * 60);
/// Events that wait for a slow connection before the oldest are lost.
const BACKLOG: usize = 1024;
/// How long a ring lasts at most, and when the plugin says 0.
const RING_MAX: u32 = 300;
const RING_DEFAULT: u32 = 60;
/// Rings one sender starts in one plugin per minute (each person counts).
const RINGS_PER_MINUTE: u32 = 60;
/// The largest badge.
const BADGE_MAX: u32 = 9999;

/// A user a ring comes from, as the panel shows them: by username, which
/// nobody changes, not by a display name anyone could pick.
#[derive(Debug, Clone, Serialize)]
pub struct From {
    pub id: Uuid,
    pub username: String,
}

/// A ring the panel shows.
#[derive(Debug, Clone, Serialize)]
pub struct Ring {
    pub id: Uuid,
    pub app: String,
    pub from: From,
    /// `call`, `message` or `invite`.
    pub kind: &'static str,
    #[serde(rename = "ref")]
    pub reference: String,
    pub until: DateTime<Utc>,
}

/// One event for one user.
#[derive(Debug, Clone)]
enum Kind {
    Changed { app: String },
    Badge { app: String, count: u32 },
    Ring(Ring),
    RingEnd { id: Uuid },
}

#[derive(Debug, Clone)]
struct Note {
    user: Uuid,
    kind: Kind,
}

/// A ring that is on, by plugin, recipient, sender and `ref`.
type RingKey = (String, Uuid, Uuid, String);

pub struct AppEvents {
    db: DatabaseConnection,
    notes: broadcast::Sender<Note>,
    open: Mutex<HashMap<Uuid, usize>>,
    rings: Mutex<HashMap<RingKey, Ring>>,
    /// Rings each sender started this minute, by plugin and sender.
    rung: Mutex<HashMap<(String, Uuid), (Instant, u32)>>,
}

impl AppEvents {
    pub fn new(db: DatabaseConnection) -> Self {
        let (notes, _) = broadcast::channel(BACKLOG);
        Self { db, notes, open: Mutex::default(), rings: Mutex::default(), rung: Mutex::default() }
    }

    fn send(&self, user: Uuid, kind: Kind) {
        // Nobody listening is fine.
        let _ = self.notes.send(Note { user, kind });
    }

    /// A job someone else added for `user` in `app` is done.
    pub fn changed(&self, user: Uuid, app: &str) {
        self.send(user, Kind::Changed { app: app.to_owned() });
    }

    /// Sets the number by `app` in `user`'s side panel; 0 hides it.
    pub async fn badge(&self, user: Uuid, app: &str, count: u32) -> Result<()> {
        let count = count.min(BADGE_MAX);
        if count == 0 {
            plugin_badge::Entity::delete_by_id((app.to_owned(), user)).exec(&self.db).await?;
        } else {
            let row = plugin_badge::ActiveModel {
                app_id: Set(app.to_owned()),
                user_id: Set(user),
                count: Set(count as i32),
                updated_at: Set(Utc::now()),
            };
            use plugin_badge::Column as C;
            plugin_badge::Entity::insert(row)
                .on_conflict(
                    OnConflict::columns([C::AppId, C::UserId]).update_columns([C::Count, C::UpdatedAt]).to_owned(),
                )
                .exec_without_returning(&self.db)
                .await?;
        }
        self.send(user, Kind::Badge { app: app.to_owned(), count });
        Ok(())
    }

    /// Counts the rings a sender starts; false if that is more than they may this minute.
    pub fn count_rings(&self, app: &str, sender: Uuid, rings: u32) -> bool {
        let mut rung = self.rung.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        rung.retain(|_, (since, _)| now.duration_since(*since) < Duration::from_secs(60));
        let (_, count) = rung.entry((app.to_owned(), sender)).or_insert((now, 0));
        if *count + rings > RINGS_PER_MINUTE {
            return false;
        }
        *count += rings;
        true
    }

    /// Rings each of `to` for `ttl` seconds (0: the default), anew if the
    /// same sender rang them with this `ref` already.
    pub fn ring(self: &Arc<Self>, app: &str, from: From, to: &[Uuid], kind: &'static str, reference: &str, ttl: u32) {
        let ttl = if ttl == 0 { RING_DEFAULT } else { ttl.clamp(5, RING_MAX) };
        let until = Utc::now() + chrono::TimeDelta::seconds(i64::from(ttl));
        for user in to {
            let ring = Ring {
                id: Uuid::new_v4(),
                app: app.to_owned(),
                from: from.clone(),
                kind,
                reference: reference.to_owned(),
                until,
            };
            let key = (app.to_owned(), *user, from.id, reference.to_owned());
            let replaced = self.rings().insert(key.clone(), ring.clone());
            if let Some(old) = replaced {
                self.send(*user, Kind::RingEnd { id: old.id });
            }
            self.send(*user, Kind::Ring(ring.clone()));
            let events = self.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_secs(u64::from(ttl))).await;
                events.end_where(|k, r| *k == key && r.id == ring.id);
            });
        }
    }

    /// Ends the rings `ref` of `sender` in `app` for each of `to`.
    pub fn cancel(&self, app: &str, sender: Uuid, to: &[Uuid], reference: &str) {
        self.end_where(|(a, user, from, r), _| a == app && *from == sender && r == reference && to.contains(user));
    }

    /// Ends one ring of `user` (they declined it in the panel), in all their tabs.
    pub fn decline(&self, user: Uuid, id: Uuid) {
        self.end_where(|key, ring| key.1 == user && ring.id == id);
    }

    /// Ends the rings `ref` in `app` for `user`, whoever rang.
    pub fn dismiss(&self, app: &str, user: Uuid, reference: &str) {
        self.end_where(|(a, u, _, r), _| a == app && *u == user && r == reference);
    }

    fn end_where(&self, matches: impl Fn(&RingKey, &Ring) -> bool) {
        let mut ended = Vec::new();
        self.rings().retain(|key, ring| {
            let end = matches(key, ring);
            if end {
                ended.push((key.1, ring.id));
            }
            !end
        });
        for (user, id) in ended {
            self.send(user, Kind::RingEnd { id });
        }
    }

    fn rings(&self) -> std::sync::MutexGuard<'_, HashMap<RingKey, Ring>> {
        self.rings.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// What a new connection of `user` starts with: badges and rings on.
    async fn current(&self, user: Uuid) -> Result<Vec<Event>> {
        let badges = plugin_badge::Entity::find().filter(plugin_badge::Column::UserId.eq(user)).all(&self.db).await?;
        let mut events: Vec<Event> =
            badges.into_iter().map(|b| event(&Kind::Badge { app: b.app_id, count: b.count.max(0) as u32 })).collect();
        let now = Utc::now();
        let rings: Vec<Ring> =
            self.rings().iter().filter(|(k, r)| k.1 == user && r.until > now).map(|(_, r)| r.clone()).collect();
        events.extend(rings.into_iter().map(|ring| event(&Kind::Ring(ring))));
        Ok(events)
    }

    fn watch(self: &Arc<Self>, user: Uuid) -> Result<Watch> {
        let mut open = self.open.lock().unwrap_or_else(|e| e.into_inner());
        let count = open.entry(user).or_default();
        if *count >= MAX_CONNECTIONS {
            return Err(Error::new(
                ErrorKind::TooManyRequests,
                "TOO_MANY_WATCHERS",
                "Too many connections; close a tab",
            ));
        }
        *count += 1;
        Ok(Watch { notes: self.notes.subscribe(), user, events: self.clone() })
    }
}

/// One connection of a user, counted while it is open.
struct Watch {
    notes: broadcast::Receiver<Note>,
    user: Uuid,
    events: Arc<AppEvents>,
}

impl Drop for Watch {
    fn drop(&mut self) {
        let mut open = self.events.open.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(count) = open.get_mut(&self.user) {
            *count -= 1;
            if *count == 0 {
                open.remove(&self.user);
            }
        }
    }
}

fn event(kind: &Kind) -> Event {
    let (name, data) = match kind {
        Kind::Changed { app } => ("changed", serde_json::json!({ "app": app })),
        Kind::Badge { app, count } => ("badge", serde_json::json!({ "app": app, "count": count })),
        Kind::Ring(ring) => ("ring", serde_json::to_value(ring).unwrap_or_default()),
        Kind::RingEnd { id } => ("ring-end", serde_json::json!({ "id": id })),
    };
    Event::default().event(name).data(data.to_string())
}

pub fn router() -> Router<AppState> {
    Router::new().route("/api/v1/apps/events", get(watch)).route("/api/v1/apps/rings/{id}", delete(decline))
}

/// `DELETE /api/v1/apps/rings/{id}`: the user declined a ring; it goes from all their tabs.
async fn decline(State(state): State<AppState>, current: CurrentUser, Path(id): Path<Uuid>) -> StatusCode {
    state.jobs.events.decline(current.user().id, id);
    StatusCode::NO_CONTENT
}

/// `GET /api/v1/apps/events`: the events of the user's plugins, until
/// [`CONNECTION_TIME`]; the panel connects again.
async fn watch(State(state): State<AppState>, current: CurrentUser) -> Result<impl IntoResponse> {
    let events = state.jobs.events.clone();
    let user = current.user().id;
    let watch = events.watch(user)?;
    let first = events.current(user).await?;
    let deadline = tokio::time::Instant::now() + CONNECTION_TIME;
    let start = futures_util::stream::iter(first.into_iter().map(Ok::<_, Infallible>));
    let rest = futures_util::stream::unfold(watch, move |mut watch| async move {
        loop {
            let note = tokio::time::timeout_at(deadline, watch.notes.recv()).await.ok()?;
            match note {
                Ok(note) if note.user == watch.user => return Some((Ok(event(&note.kind)), watch)),
                Ok(_) | Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => return None,
            }
        }
    });
    let headers = [("x-accel-buffering", "no"), (header::CACHE_CONTROL.as_str(), "no-store")];
    Ok((headers, Sse::new(futures_util::StreamExt::chain(start, rest)).keep_alive(KeepAlive::default())))
}
