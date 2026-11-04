//! Background jobs of plugins: indexing, reading photo metadata, finding
//! faces. A plugin adds jobs for the user of a request (`jobs` import) —
//! one job per thing to do — and the server runs them later through its
//! `run-batch` export, as that user, in batches of the size its manifest
//! (or the administrator) sets for the kind.
//!
//! [`PluginSettings::workers`] batches run at a time. The next batch is
//! chosen in turn: plugin, then user within it, then kind within that, so
//! one user's 50 000 photos or a slow neural network don't hold up the rest.
//! A job the plugin can't do now is tried again later, five times in all;
//! the jobs of a batch that failed as a whole (a panic, a limit) are tried
//! one by one, so one broken file doesn't sink the others. Done jobs are
//! deleted; failed ones stay, with the reason, until added again.
//!
//! Jobs also start by themselves: a kind with `on` in the manifest gets a
//! job for each file the user gives the plugin with a folder
//! (`file.granted`), and for each file created, changed, deleted or moved
//! there later (`file.created`, `file.changed`, `file.deleted`,
//! `file.moved`), filtered by `mime`. Changes the plugin makes itself don't
//! start its own jobs.
//!
//! Every batch writes a line to the server log: plugin, kind, user, size,
//! what became of it, how many jobs are left — if that number doesn't go
//! down, something is wrong.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{TimeDelta, Utc};
use rustcloud_auth::Auth;
use rustcloud_core::{Error, Result};
use rustcloud_db::{plugin_job, DatabaseConnection};
use rustcloud_files::{FileEvent, Files, Grant, GrantKind};
use rustcloud_wasm_host::{Job, JobOutcome, JobProgress, NewJob};
use sea_orm::sea_query::{Expr, OnConflict};
use sea_orm::ActiveValue::{NotSet, Unchanged};
use sea_orm::{ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder, QuerySelect, Set};
use serde::Serialize;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::{broadcast, Notify};
use uuid::Uuid;

use crate::app_links::AppLinks;
use crate::app_routes::caller_for;
use crate::plugins::Plugins;

/// Tries of a job before it counts as failed.
const MAX_ATTEMPTS: i32 = 5;
/// The first retry waits this long, each next one twice as long.
const RETRY_AFTER: Duration = Duration::from_secs(30);
/// Longest a key and the data of a job may be, and most jobs added at once.
const KEY_LIMIT: usize = 512;
const DATA_LIMIT: usize = 4096;
const ADD_LIMIT: usize = 1000;
/// Jobs of one plugin for one user that may wait at once: a plugin (or a
/// mistake in one) can't fill the database with them.
const WAITING_LIMIT: u64 = 200_000;
/// How often an idle worker looks for jobs whose retry time has come.
const IDLE: Duration = Duration::from_secs(10);

const WAITING: &str = "waiting";
const RUNNING: &str = "running";
const FAILED: &str = "failed";

/// How many jobs of a kind are left and failed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Counts {
    /// Not done yet, running ones included.
    pub waiting: u64,
    pub failed: u64,
}

/// The jobs of every plugin that has any: plugin id → kind → counts.
pub async fn counts(db: &DatabaseConnection) -> Result<HashMap<String, BTreeMap<String, Counts>>> {
    let rows: Vec<(String, String, String, i64)> = plugin_job::Entity::find()
        .select_only()
        .columns([plugin_job::Column::AppId, plugin_job::Column::Kind, plugin_job::Column::State])
        .column_as(plugin_job::Column::Id.count(), "count")
        .group_by(plugin_job::Column::AppId)
        .group_by(plugin_job::Column::Kind)
        .group_by(plugin_job::Column::State)
        .into_tuple()
        .all(db)
        .await?;
    let mut counts: HashMap<String, BTreeMap<String, Counts>> = HashMap::new();
    for (app_id, kind, state, count) in rows {
        let entry = counts.entry(app_id).or_default().entry(kind).or_default();
        match state.as_str() {
            FAILED => entry.failed += count as u64,
            _ => entry.waiting += count as u64,
        }
    }
    Ok(counts)
}

/// Deletes the jobs of a plugin (it is removed).
pub async fn forget_app(db: &DatabaseConnection, app_id: &str) -> Result<()> {
    plugin_job::Entity::delete_many().filter(plugin_job::Column::AppId.eq(app_id)).exec(db).await?;
    Ok(())
}

pub struct Jobs {
    db: DatabaseConnection,
    files: Arc<Files>,
    auth: Arc<Auth>,
    plugins: Arc<Plugins>,
    links: Arc<AppLinks>,
    /// Wakes idle workers when jobs are added.
    wake: Notify,
    /// Whose turn it is; taken while a batch is being chosen.
    turn: tokio::sync::Mutex<Turn>,
    /// Batches running now, by plugin: each runs at most its `workers`.
    running: std::sync::Mutex<HashMap<String, usize>>,
}

/// A batch chosen to run: jobs of one plugin, user and kind.
struct Batch {
    app_id: String,
    user_id: Uuid,
    kind: String,
    rows: Vec<plugin_job::Model>,
}

impl Jobs {
    pub fn new(
        db: DatabaseConnection,
        files: Arc<Files>,
        auth: Arc<Auth>,
        plugins: Arc<Plugins>,
        links: Arc<AppLinks>,
    ) -> Self {
        let (wake, turn, running) = (Notify::new(), Default::default(), Default::default());
        Self { db, files, auth, plugins, links, wake, turn, running }
    }

    /// Starts `workers` workers and the watcher of file events. Jobs that
    /// were running when the server stopped run again.
    pub fn start(self: &Arc<Self>, workers: usize) {
        tokio::spawn(self.clone().watch(self.files.subscribe()));
        let jobs = self.clone();
        tokio::spawn(async move {
            let reset = plugin_job::Entity::update_many()
                .col_expr(plugin_job::Column::State, Expr::value(WAITING))
                .filter(plugin_job::Column::State.eq(RUNNING))
                .exec(&jobs.db)
                .await;
            if let Err(err) = reset {
                tracing::warn!("plugin jobs: {err}");
            }
            tracing::info!("plugin jobs: {workers} at a time");
            for _ in 0..workers {
                tokio::spawn(jobs.clone().work());
            }
        });
    }

    /// Adds jobs of a plugin for a user; a job with the kind and key of an
    /// existing one replaces it.
    pub async fn add(&self, app_id: &str, user_id: Uuid, jobs: Vec<NewJob>) -> Result<()> {
        let manifest = self.plugins.get(app_id).ok_or_else(|| Error::not_found("APP_NOT_FOUND", "No such app"))?;
        if jobs.len() > ADD_LIMIT {
            return Err(invalid_job(format!("more than {ADD_LIMIT} jobs at once")));
        }
        for job in &jobs {
            if !manifest.jobs.iter().any(|kind| kind.kind == job.kind) {
                return Err(Error::bad_request("UNKNOWN_JOB_KIND", job.kind.clone()));
            }
            if job.key.is_empty() || job.key.len() > KEY_LIMIT || job.data.len() > DATA_LIMIT {
                return Err(invalid_job(format!(
                    "a key is 1–{KEY_LIMIT} bytes and data at most {DATA_LIMIT}: \"{}\"",
                    job.key.chars().take(40).collect::<String>()
                )));
            }
        }
        if jobs.is_empty() {
            return Ok(());
        }
        let waiting = plugin_job::Entity::find()
            .filter(plugin_job::Column::AppId.eq(app_id))
            .filter(plugin_job::Column::UserId.eq(user_id))
            .count(&self.db)
            .await?;
        if waiting + jobs.len() as u64 > WAITING_LIMIT {
            return Err(Error::conflict("TOO_MANY_JOBS", format!("more than {WAITING_LIMIT} jobs would wait")));
        }
        let now = Utc::now();
        let rows = jobs.into_iter().map(|job| plugin_job::ActiveModel {
            id: Set(Uuid::now_v7()),
            app_id: Set(app_id.to_owned()),
            user_id: Set(user_id),
            kind: Set(job.kind),
            key: Set(job.key),
            data: Set(job.data),
            state: Set(WAITING.to_owned()),
            attempts: Set(0),
            alone: Set(false),
            error: Set(None),
            run_after: Set(now),
            created_at: Set(now),
            updated_at: Set(now),
        });
        use plugin_job::Column as C;
        plugin_job::Entity::insert_many(rows)
            .on_conflict(
                OnConflict::columns([C::AppId, C::UserId, C::Kind, C::Key])
                    .update_columns([C::Data, C::State, C::Attempts, C::Alone, C::Error, C::RunAfter, C::UpdatedAt])
                    .to_owned(),
            )
            .exec(&self.db)
            .await?;
        self.wake.notify_waiters();
        Ok(())
    }

    /// Adds the jobs the files of a new folder grant start (`file.granted`),
    /// in the background: a large folder takes a while.
    pub fn on_grant(self: &Arc<Self>, user: rustcloud_auth::User, grant: Grant) {
        if grant.kind != GrantKind::Standing {
            return;
        }
        let Some(manifest) = self.plugins.get(&grant.app_id) else {
            return;
        };
        if !manifest.jobs.iter().any(|kind| kind.on.iter().any(|on| on == "file.granted")) {
            return;
        }
        let jobs = self.clone();
        tokio::spawn(async move {
            let added = async {
                let mut new = Vec::new();
                for file in jobs.files.granted_files(&user, grant.id).await? {
                    for kind in manifest.jobs.iter().filter(|k| k.wants("file.granted", file.mime_type.as_deref())) {
                        new.push(NewJob {
                            kind: kind.kind.clone(),
                            key: file.id.to_string(),
                            data: "file.granted".to_owned(),
                        });
                    }
                }
                let count = new.len();
                for chunk in new.chunks(ADD_LIMIT) {
                    jobs.add(&grant.app_id, user.id, chunk.to_vec()).await?;
                }
                Ok::<_, Error>(count)
            };
            match added.await {
                Ok(count) => tracing::info!(
                    app = grant.app_id,
                    user = user.username,
                    "plugin jobs: {count} added for the files {} was given",
                    grant.app_id
                ),
                Err(err) => tracing::warn!(app = grant.app_id, "plugin jobs: {err}"),
            }
        });
    }

    /// Adds jobs for what happens to files, as long as the server runs.
    async fn watch(self: Arc<Self>, mut events: broadcast::Receiver<FileEvent>) {
        loop {
            match events.recv().await {
                Ok(event) => {
                    if let Err(err) = self.on_file_event(event).await {
                        tracing::warn!("plugin jobs: {err}");
                    }
                }
                Err(RecvError::Lagged(lost)) => {
                    tracing::warn!("plugin jobs: {lost} file events were lost, too many at once")
                }
                Err(RecvError::Closed) => return,
            }
        }
    }

    /// Adds the jobs a file event starts in the plugins that watch its files.
    async fn on_file_event(&self, event: FileEvent) -> Result<()> {
        let name = event.kind.name();
        for manifest in self.plugins.running_manifests() {
            if event.app_id.as_deref() == Some(manifest.id.as_str()) {
                continue;
            }
            let kinds: Vec<_> = manifest.jobs.iter().filter(|kind| kind.on.iter().any(|on| on == name)).collect();
            if kinds.is_empty() {
                continue;
            }
            let mut new = Vec::new();
            for file in event.files.iter().filter(|file| file.apps.contains(&manifest.id)) {
                for kind in kinds.iter().filter(|kind| kind.wants(name, file.node.mime_type.as_deref())) {
                    new.push(NewJob { kind: kind.kind.clone(), key: file.node.id.to_string(), data: name.to_owned() });
                }
            }
            for chunk in new.chunks(ADD_LIMIT) {
                self.add(&manifest.id, event.owner_id, chunk.to_vec()).await?;
            }
        }
        Ok(())
    }

    /// A user's jobs of a plugin, by kind.
    pub async fn progress(&self, app_id: &str, user_id: Uuid) -> Result<Vec<JobProgress>> {
        let rows: Vec<(String, String, i64)> = plugin_job::Entity::find()
            .select_only()
            .columns([plugin_job::Column::Kind, plugin_job::Column::State])
            .column_as(plugin_job::Column::Id.count(), "count")
            .filter(plugin_job::Column::AppId.eq(app_id))
            .filter(plugin_job::Column::UserId.eq(user_id))
            .group_by(plugin_job::Column::Kind)
            .group_by(plugin_job::Column::State)
            .into_tuple()
            .all(&self.db)
            .await?;
        let mut kinds: BTreeMap<String, JobProgress> = BTreeMap::new();
        for (kind, state, count) in rows {
            let entry = kinds.entry(kind.clone()).or_insert(JobProgress { kind, waiting: 0, failed: 0 });
            match state.as_str() {
                FAILED => entry.failed += count as u64,
                _ => entry.waiting += count as u64,
            }
        }
        Ok(kinds.into_values().collect())
    }

    /// Deletes a user's jobs of a plugin (the user took back all access).
    pub async fn forget_user(&self, app_id: &str, user_id: Uuid) -> Result<()> {
        plugin_job::Entity::delete_many()
            .filter(plugin_job::Column::AppId.eq(app_id))
            .filter(plugin_job::Column::UserId.eq(user_id))
            .exec(&self.db)
            .await?;
        Ok(())
    }

    /// One worker: runs batches while there are any, then waits.
    async fn work(self: Arc<Self>) {
        loop {
            // Listening before looking: jobs added in between still wake us.
            let wake = self.wake.notified();
            tokio::pin!(wake);
            wake.as_mut().enable();
            match self.next_batch().await {
                Ok(Some(batch)) => {
                    let app_id = batch.app_id.clone();
                    self.run(batch).await;
                    if let Some(count) = self.running().get_mut(&app_id) {
                        *count = count.saturating_sub(1);
                    }
                    // A plugin at its limit may have batches waiting for this one.
                    self.wake.notify_waiters();
                }
                Ok(None) => {
                    tokio::select! {
                        _ = wake => {}
                        _ = tokio::time::sleep(IDLE) => {}
                    }
                }
                Err(err) => {
                    tracing::warn!("plugin jobs: {err}");
                    tokio::time::sleep(IDLE).await;
                }
            }
        }
    }

    /// Takes the next batch in turn and marks it running.
    async fn next_batch(&self) -> Result<Option<Batch>> {
        let mut turn = self.turn.lock().await;
        let now = Utc::now();
        let ready: Vec<(String, Uuid, String)> = plugin_job::Entity::find()
            .select_only()
            .columns([plugin_job::Column::AppId, plugin_job::Column::UserId, plugin_job::Column::Kind])
            .filter(plugin_job::Column::State.eq(WAITING))
            .filter(plugin_job::Column::RunAfter.lte(now))
            .distinct()
            .into_tuple()
            .all(&self.db)
            .await?;
        // Only plugins that run, below their number of batches at a time,
        // and kinds their manifest still has.
        let running = self.running().clone();
        let ready: Vec<_> = ready
            .into_iter()
            .filter(|(app_id, _, kind)| {
                self.plugins.get(app_id).is_some_and(|m| m.jobs.iter().any(|k| &k.kind == kind))
                    && self.plugins.host().runs_jobs(app_id)
                    && running.get(app_id).copied().unwrap_or(0) < self.plugins.workers(app_id)
            })
            .collect();
        let Some((app_id, user_id, kind)) = turn.next(&ready) else {
            return Ok(None);
        };

        let (size, _) = self.plugins.job_config(&app_id, &kind);
        let mut rows = plugin_job::Entity::find()
            .filter(plugin_job::Column::AppId.eq(&app_id))
            .filter(plugin_job::Column::UserId.eq(user_id))
            .filter(plugin_job::Column::Kind.eq(&kind))
            .filter(plugin_job::Column::State.eq(WAITING))
            .filter(plugin_job::Column::RunAfter.lte(now))
            .order_by_desc(plugin_job::Column::Alone)
            .order_by_asc(plugin_job::Column::CreatedAt)
            .limit(u64::from(size))
            .all(&self.db)
            .await?;
        if rows.first().is_some_and(|row| row.alone) {
            rows.truncate(1);
        }
        let ids: Vec<Uuid> = rows.iter().map(|row| row.id).collect();
        plugin_job::Entity::update_many()
            .col_expr(plugin_job::Column::State, Expr::value(RUNNING))
            .filter(plugin_job::Column::Id.is_in(ids))
            .exec(&self.db)
            .await?;
        *self.running().entry(app_id.clone()).or_default() += 1;
        Ok(Some(Batch { app_id, user_id, kind, rows }))
    }

    fn running(&self) -> std::sync::MutexGuard<'_, HashMap<String, usize>> {
        self.running.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Runs a batch as its user and keeps what became of each job.
    async fn run(self: &Arc<Self>, batch: Batch) {
        let started = Instant::now();
        let Batch { app_id, user_id, kind, rows } = batch;
        let size = rows.len();
        let user = match self.auth.find_user(user_id).await {
            Ok(user) if !user.disabled => user,
            // A blocked user's jobs wait for the account to be unblocked.
            _ => {
                for row in rows {
                    self.keep(row, WAITING, None, Some(Duration::from_secs(3600))).await;
                }
                return;
            }
        };
        let username = user.username.clone();
        // No session: a batch makes no links for a page.
        let caller = caller_for(&self.files, self, &self.links, user, None, &app_id, false);
        let (_, config) = self.plugins.job_config(&app_id, &kind);
        let jobs = rows
            .iter()
            .map(|row| Job {
                kind: row.kind.clone(),
                key: row.key.clone(),
                data: row.data.clone(),
                attempts: row.attempts as u32,
            })
            .collect();

        let (mut done, mut failed, mut again) = (Vec::new(), 0, 0);
        match self.plugins.host().run_batch(&app_id, caller, jobs, &config).await {
            Ok(outcomes) => {
                let mut outcomes = outcomes.into_iter();
                for row in rows {
                    match outcomes.next() {
                        Some(JobOutcome::Done) => done.push(row.id),
                        Some(JobOutcome::Failed(why)) => {
                            failed += 1;
                            self.keep(row, FAILED, Some(why), None).await;
                        }
                        Some(JobOutcome::Retry(why)) => match self.retry(row, why, false).await {
                            true => again += 1,
                            false => failed += 1,
                        },
                        None => match self.retry(row, "no outcome".to_owned(), false).await {
                            true => again += 1,
                            false => failed += 1,
                        },
                    }
                }
            }
            Err(err) => {
                // One by one from now on, so a job that breaks the plugin
                // doesn't take the others with it.
                let alone = size > 1;
                for row in rows {
                    match self.retry(row, err.message.clone(), alone).await {
                        true => again += 1,
                        false => failed += 1,
                    }
                }
            }
        }
        let finished = done.len();
        if let Err(err) =
            plugin_job::Entity::delete_many().filter(plugin_job::Column::Id.is_in(done)).exec(&self.db).await
        {
            tracing::warn!(app = app_id, "plugin jobs: {err}");
        }
        let left = plugin_job::Entity::find()
            .filter(plugin_job::Column::AppId.eq(&app_id))
            .filter(plugin_job::Column::State.ne(FAILED))
            .count(&self.db)
            .await
            .unwrap_or_default();
        tracing::info!(
            app = app_id,
            kind,
            user = username,
            batch = size,
            done = finished,
            failed,
            again,
            left,
            ms = started.elapsed().as_millis() as u64,
            "plugin jobs: {app_id}/{kind} ×{size} for {username}: {finished} done, {failed} failed, \
             {again} later; {left} left"
        );
        self.wake.notify_waiters();
    }

    /// Puts a job back to be tried again, or fails it after the last try.
    /// Returns whether it will be tried again.
    async fn retry(&self, row: plugin_job::Model, why: String, alone: bool) -> bool {
        let attempts = row.attempts + 1;
        if attempts >= MAX_ATTEMPTS {
            let mut failed = row;
            failed.attempts = attempts;
            self.keep(failed, FAILED, Some(why), None).await;
            return false;
        }
        // Split off at once; otherwise wait longer each time.
        let wait = if alone && !row.alone { Duration::ZERO } else { RETRY_AFTER * 2u32.pow(attempts as u32 - 1) };
        let mut again = row;
        again.attempts = attempts;
        again.alone = again.alone || alone;
        self.keep(again, WAITING, Some(why), Some(wait)).await;
        true
    }

    /// Saves a job in `state`, with its tries, the reason and when to run it again.
    async fn keep(&self, row: plugin_job::Model, state: &str, error: Option<String>, wait: Option<Duration>) {
        let now = Utc::now();
        let job = plugin_job::ActiveModel {
            id: Unchanged(row.id),
            state: Set(state.to_owned()),
            attempts: Set(row.attempts),
            alone: Set(row.alone),
            error: Set(error),
            run_after: wait.map_or(NotSet, |wait| Set(now + TimeDelta::from_std(wait).unwrap_or_default())),
            updated_at: Set(now),
            ..Default::default()
        };
        if let Err(err) = plugin_job::Entity::update(job).exec(&self.db).await {
            tracing::warn!(app = row.app_id, "plugin jobs: {err}");
        }
    }
}

fn invalid_job(message: String) -> Error {
    Error::bad_request("INVALID_JOB", message)
}

/// Whose turn it is: the last plugin served, and for each plugin the last
/// user, and for each of those the last kind.
#[derive(Default)]
struct Turn {
    app: Option<String>,
    user: HashMap<String, Uuid>,
    kind: HashMap<(String, Uuid), String>,
}

impl Turn {
    /// The next plugin, user and kind among those with jobs `ready`.
    fn next(&mut self, ready: &[(String, Uuid, String)]) -> Option<(String, Uuid, String)> {
        let app = after(ready.iter().map(|r| r.0.clone()).collect(), self.app.as_ref())?;
        let users = ready.iter().filter(|r| r.0 == app).map(|r| r.1).collect();
        let user = after(users, self.user.get(&app))?;
        let kinds = ready.iter().filter(|r| r.0 == app && r.1 == user).map(|r| r.2.clone()).collect();
        let kind = after(kinds, self.kind.get(&(app.clone(), user)))?;
        self.app = Some(app.clone());
        self.user.insert(app.clone(), user);
        self.kind.insert((app.clone(), user), kind.clone());
        Some((app, user, kind))
    }
}

/// The first of `items` after `last`, or the first one: their turn in a circle.
fn after<T: Ord + Clone>(mut items: Vec<T>, last: Option<&T>) -> Option<T> {
    items.sort();
    items.dedup();
    last.and_then(|last| items.iter().find(|item| *item > last)).or(items.first()).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugins_users_and_kinds_take_turns() {
        let (alice, bob) = (Uuid::from_u128(1), Uuid::from_u128(2));
        let job = |app: &str, user: Uuid, kind: &str| (app.to_owned(), user, kind.to_owned());
        let ready = [
            job("faces", alice, "exif"),
            job("faces", alice, "faces"),
            job("faces", bob, "exif"),
            job("music", alice, "tags"),
        ];
        let mut turn = Turn::default();
        let order: Vec<String> = (0..6)
            .map(|_| {
                let (app, user, kind) = turn.next(&ready).unwrap();
                format!("{app}/{}/{kind}", if user == alice { "alice" } else { "bob" })
            })
            .collect();
        assert_eq!(
            order,
            [
                "faces/alice/exif",
                "music/alice/tags",
                "faces/bob/exif",
                "music/alice/tags",
                "faces/alice/faces",
                "music/alice/tags"
            ]
        );
        assert!(Turn::default().next(&[]).is_none());
    }
}
