//! Background sync between the local entries folder and the cloud.
//! The algorithm is described in `docs/cloud-sync.md`.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use chrono::{DateTime, Local, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::cloud::{
    self, CloudClient, CloudConfig, CloudError, CloudResult, EntryUpsert, PageKey, RemoteEntry,
    Session, SignUpOutcome,
};
use crate::config;
use crate::storage::{self, Store, content_hash};

const SYNC_INTERVAL: Duration = Duration::from_secs(30);
const PAGE_SIZE: usize = 500;
const PUSH_BATCH: usize = 50;
/// Re-read rows this far behind the cursor, in case they committed out of order.
const CURSOR_OVERLAP_SECONDS: i64 = 60;

// ---------------------------------------------------------------------------
// The sync engine

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Record {
    pub filename: String,
    /// Hash of the content as of the last successful sync.
    pub hash: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SyncState {
    pub cursor: Option<DateTime<Utc>>,
    pub entries: BTreeMap<Uuid, Record>,
}

impl SyncState {
    pub fn path_for(user_id: Uuid) -> PathBuf {
        config::dir().join(format!("sync-state-{user_id}.json"))
    }

    pub fn load(path: &Path) -> Self {
        fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) {
        if let Ok(json) = serde_json::to_string(self) {
            let tmp = path.with_extension("json.tmp");
            if fs::write(&tmp, json).is_ok() {
                let _ = fs::rename(tmp, path);
            }
        }
    }

    fn record(&mut self, id: Uuid, filename: &str, hash: String) {
        let filename = filename.to_string();
        self.entries.insert(id, Record { filename, hash });
    }
}

/// The server side of a sync, so the engine can be tested without a network.
pub trait Remote {
    /// Rows after `after` in `(updated_at, id)` order.
    fn fetch(&self, after: Option<PageKey>, limit: usize) -> CloudResult<Vec<RemoteEntry>>;
    fn upsert(&self, rows: &[EntryUpsert]) -> CloudResult<()>;
}

struct Supabase<'a> {
    client: &'a CloudClient,
    session: &'a Session,
}

impl Remote for Supabase<'_> {
    fn fetch(&self, after: Option<PageKey>, limit: usize) -> CloudResult<Vec<RemoteEntry>> {
        self.client.fetch_changes(self.session, after, limit)
    }

    fn upsert(&self, rows: &[EntryUpsert]) -> CloudResult<()> {
        self.client.upsert(self.session, rows)
    }
}

/// Entries whose local files a sync created, rewrote or deleted.
#[derive(Debug, Default)]
pub struct Changes {
    pub changed: HashSet<Uuid>,
    pub removed: HashSet<Uuid>,
}

impl Changes {
    pub fn is_empty(&self) -> bool {
        self.changed.is_empty() && self.removed.is_empty()
    }

    fn merge(&mut self, other: Changes) {
        self.changed.extend(other.changed);
        self.removed.extend(other.removed);
    }
}

pub struct Engine<'a> {
    pub store: &'a Store,
    pub remote: &'a dyn Remote,
    pub user_id: Uuid,
    /// The bundled guide; an untouched copy of it is never uploaded.
    pub welcome: &'a str,
}

impl Engine<'_> {
    pub fn run(&self, state: &mut SyncState) -> CloudResult<Changes> {
        let mut changes = Changes::default();
        let cursor = self.pull(state, &mut changes)?;
        self.push(state)?;
        state.cursor = cursor;
        Ok(changes)
    }

    fn pull(
        &self,
        state: &mut SyncState,
        changes: &mut Changes,
    ) -> CloudResult<Option<DateTime<Utc>>> {
        let mut after = state.cursor.map(|cursor| PageKey {
            updated_at: cursor - chrono::Duration::seconds(CURSOR_OVERLAP_SECONDS),
            id: None,
        });
        let mut newest = state.cursor;
        loop {
            let page = self.remote.fetch(after, PAGE_SIZE)?;
            for row in &page {
                self.apply(row, state, changes)
                    .map_err(|e| CloudError::Server(format!("Couldn't save an entry: {e}")))?;
                newest = newest.max(Some(row.updated_at));
            }
            let Some(last) = page.last().filter(|_| page.len() == PAGE_SIZE) else {
                return Ok(newest);
            };
            after = Some(PageKey {
                updated_at: last.updated_at,
                id: Some(last.id),
            });
        }
    }

    fn apply(
        &self,
        row: &RemoteEntry,
        state: &mut SyncState,
        changes: &mut Changes,
    ) -> std::io::Result<()> {
        // The filename becomes a path; only accept the exact entry format.
        let Some((id, created)) = storage::parse_filename(&row.filename) else {
            return Ok(());
        };
        if id != row.id {
            return Ok(());
        }

        let record = state.entries.get(&id).cloned();
        let local = self
            .store
            .read(&row.filename)
            .ok()
            .map(|content| content_hash(&content));
        let remote_hash = content_hash(&row.content);

        if row.deleted {
            match local {
                None => {
                    state.entries.remove(&id);
                }
                // Unchanged here since the last sync: follow the deletion.
                Some(local) if record.as_ref().is_some_and(|r| r.hash == local) => {
                    if self.store.delete_if_unchanged(&row.filename, &local)? {
                        state.entries.remove(&id);
                        changes.removed.insert(id);
                    }
                }
                // Edited here since: keep it, and the push resurrects it.
                Some(_) => {}
            }
            return Ok(());
        }

        // Nothing changed remotely since we last synced this entry.
        if record.as_ref().is_some_and(|r| r.hash == remote_hash) {
            return Ok(());
        }

        let Some(local) = local else {
            if self
                .store
                .write_if_unchanged(&row.filename, None, &row.content)?
            {
                state.record(id, &row.filename, remote_hash);
                changes.changed.insert(id);
            }
            return Ok(());
        };

        if local == remote_hash {
            state.record(id, &row.filename, remote_hash);
            return Ok(());
        }

        let locally_edited = record.is_none_or(|r| r.hash != local);
        if !locally_edited
            && self
                .store
                .write_if_unchanged(&row.filename, Some(&local), &row.content)?
        {
            state.record(id, &row.filename, remote_hash);
            changes.changed.insert(id);
            return Ok(());
        }

        // Both sides changed. Keep ours in place and the other one next to it,
        // so no writing is lost; ours gets pushed because its hash differs.
        if !self.is_disposable(&row.content) {
            let copy_id = Uuid::new_v4();
            let copy = storage::filename_for(copy_id, created);
            if self.store.write_if_unchanged(&copy, None, &row.content)? {
                changes.changed.insert(copy_id);
            }
        }
        state.record(id, &row.filename, remote_hash);
        Ok(())
    }

    fn push(&self, state: &mut SyncState) -> CloudResult<()> {
        // An unreadable folder (unmounted drive, moved Documents) must not
        // look like every entry was deleted.
        let local = self
            .store
            .by_id()
            .map_err(|e| CloudError::Server(format!("Couldn't read the Freewrite folder: {e}")))?;
        let mut rows = Vec::new();

        for (id, (entry, content)) in &local {
            let hash = content_hash(content);
            match state.entries.get(id) {
                Some(record) if record.hash == hash => continue,
                // Every device starts with a blank page and the guide; don't upload those.
                None if self.is_disposable(content) => continue,
                _ => {}
            }
            let modified = self
                .store
                .modified(&entry.filename)
                .map(DateTime::<Utc>::from)
                .unwrap_or_else(Utc::now);
            let row = EntryUpsert {
                user_id: self.user_id,
                id: *id,
                filename: entry.filename.clone(),
                content: content.clone(),
                deleted: false,
                client_updated_at: modified,
            };
            rows.push((row, Some(hash)));
        }

        for (id, record) in &state.entries {
            if !local.contains_key(id) {
                let row = EntryUpsert {
                    user_id: self.user_id,
                    id: *id,
                    filename: record.filename.clone(),
                    content: String::new(),
                    deleted: true,
                    client_updated_at: Utc::now(),
                };
                rows.push((row, None));
            }
        }

        for batch in rows.chunks(PUSH_BATCH) {
            let upserts: Vec<EntryUpsert> = batch.iter().map(|(row, _)| row.clone()).collect();
            self.remote.upsert(&upserts)?;
            for (row, hash) in batch {
                match hash {
                    Some(hash) => state.record(row.id, &row.filename, hash.clone()),
                    None => {
                        state.entries.remove(&row.id);
                    }
                }
            }
        }
        Ok(())
    }

    fn is_disposable(&self, content: &str) -> bool {
        let trimmed = content.trim();
        trimmed.is_empty() || trimmed == self.welcome.trim()
    }
}

// ---------------------------------------------------------------------------
// The background worker

#[derive(Clone, Debug, Default)]
pub struct AccountStatus {
    /// Whether a Supabase project is configured at all.
    pub configured: bool,
    pub email: Option<String>,
    /// Signing in or creating an account.
    pub busy: bool,
    pub syncing: bool,
    pub last_synced: Option<DateTime<Local>>,
    /// Informational, e.g. "check your email".
    pub notice: Option<String>,
    pub error: Option<String>,
}

enum Command {
    SignIn { email: String, password: String },
    SignUp { email: String, password: String },
    SignOut,
    SyncNow,
    Shutdown,
}

pub struct SyncHandle {
    commands: Sender<Command>,
    changes: Receiver<Changes>,
    status: Arc<Mutex<AccountStatus>>,
    finished: Receiver<()>,
}

impl SyncHandle {
    pub fn spawn(store: Arc<Store>, welcome: String, ctx: egui::Context) -> Self {
        let (commands, command_rx) = mpsc::channel();
        let (changes_tx, changes) = mpsc::channel();
        let (finished_tx, finished) = mpsc::channel();
        let config = CloudConfig::load();
        let status = Arc::new(Mutex::new(AccountStatus {
            configured: config.is_some(),
            ..Default::default()
        }));

        if let Some(config) = config {
            let worker = Worker {
                client: CloudClient::new(config),
                session: None,
                store,
                welcome,
                status: status.clone(),
                changes: changes_tx,
                ctx,
            };
            thread::Builder::new()
                .name("freewrite-sync".into())
                .spawn(move || {
                    worker.run(command_rx);
                    let _ = finished_tx.send(());
                })
                .expect("sync thread");
        }

        SyncHandle {
            commands,
            changes,
            status,
            finished,
        }
    }

    pub fn status(&self) -> AccountStatus {
        self.status
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn sign_in(&self, email: String, password: String) {
        let _ = self.commands.send(Command::SignIn { email, password });
    }

    pub fn sign_up(&self, email: String, password: String) {
        let _ = self.commands.send(Command::SignUp { email, password });
    }

    pub fn sign_out(&self) {
        let _ = self.commands.send(Command::SignOut);
    }

    pub fn sync_now(&self) {
        let _ = self.commands.send(Command::SyncNow);
    }

    /// Everything synced since the last call.
    pub fn take_changes(&self) -> Option<Changes> {
        let mut all: Option<Changes> = None;
        while let Ok(changes) = self.changes.try_recv() {
            all.get_or_insert_with(Changes::default).merge(changes);
        }
        all
    }

    /// Run a last sync, waiting at most `timeout` so quitting never hangs.
    pub fn shutdown(&self, timeout: Duration) {
        if self.commands.send(Command::Shutdown).is_ok() {
            let _ = self.finished.recv_timeout(timeout);
        }
    }
}

struct Worker {
    client: CloudClient,
    session: Option<Session>,
    store: Arc<Store>,
    welcome: String,
    status: Arc<Mutex<AccountStatus>>,
    changes: Sender<Changes>,
    ctx: egui::Context,
}

impl Worker {
    fn run(mut self, commands: Receiver<Command>) {
        if let Some(saved) = cloud::load_session() {
            self.update(|s| s.email = Some(saved.email.clone()));
            self.session = Some(saved);
            self.sync();
        }

        loop {
            match commands.recv_timeout(SYNC_INTERVAL) {
                Ok(Command::SignIn { email, password }) => {
                    self.authenticate(|client| client.sign_in(&email, &password).map(Some));
                }
                Ok(Command::SignUp { email, password }) => {
                    self.authenticate(|client| match client.sign_up(&email, &password)? {
                        SignUpOutcome::SignedIn(session) => Ok(Some(session)),
                        SignUpOutcome::ConfirmEmail => Ok(None),
                    });
                }
                Ok(Command::SignOut) => {
                    if let Some(session) = self.session.take() {
                        self.client.sign_out(&session);
                    }
                    cloud::clear_session();
                    self.update(|s| {
                        *s = AccountStatus {
                            configured: true,
                            ..Default::default()
                        }
                    });
                }
                Ok(Command::SyncNow) | Err(RecvTimeoutError::Timeout) => self.sync(),
                Ok(Command::Shutdown) | Err(RecvTimeoutError::Disconnected) => {
                    self.sync();
                    return;
                }
            }
        }
    }

    fn update(&self, change: impl FnOnce(&mut AccountStatus)) {
        change(&mut self.status.lock().unwrap_or_else(|e| e.into_inner()));
        self.ctx.request_repaint();
    }

    fn authenticate(&mut self, attempt: impl FnOnce(&CloudClient) -> CloudResult<Option<Session>>) {
        self.update(|s| {
            s.busy = true;
            s.error = None;
            s.notice = None;
        });
        match attempt(&self.client) {
            Ok(Some(session)) => {
                cloud::save_session(&session);
                let email = session.email.clone();
                self.session = Some(session);
                self.update(|s| {
                    s.busy = false;
                    s.email = Some(email);
                });
                self.sync();
            }
            Ok(None) => self.update(|s| {
                s.busy = false;
                s.notice = Some("Check your email to confirm your account, then sign in.".into());
            }),
            Err(err) => self.update(|s| {
                s.busy = false;
                s.error = Some(err.to_string());
            }),
        }
    }

    fn sync(&mut self) {
        if self.session.is_none() {
            return;
        }
        self.update(|s| s.syncing = true);
        let result = self.sync_once().or_else(|err| match err {
            // The access token may have been revoked early; refresh and retry once.
            CloudError::Unauthorized(_) => {
                self.refresh()?;
                self.sync_once()
            }
            other => Err(other),
        });

        match result {
            Ok(changes) => {
                if !changes.is_empty() {
                    let _ = self.changes.send(changes);
                }
                self.update(|s| {
                    s.syncing = false;
                    s.error = None;
                    s.last_synced = Some(Local::now());
                });
            }
            Err(CloudError::Unauthorized(_)) => {
                self.session = None;
                cloud::clear_session();
                self.update(|s| {
                    *s = AccountStatus {
                        configured: true,
                        error: Some("Your session expired. Please sign in again.".into()),
                        ..Default::default()
                    }
                });
            }
            Err(err) => self.update(|s| {
                s.syncing = false;
                s.error = Some(err.to_string());
            }),
        }
    }

    fn refresh(&mut self) -> CloudResult<()> {
        let Some(session) = &self.session else {
            return Err(CloudError::Unauthorized("Signed out".into()));
        };
        let fresh = self.client.refresh(session)?;
        cloud::save_session(&fresh);
        self.session = Some(fresh);
        Ok(())
    }

    fn sync_once(&mut self) -> CloudResult<Changes> {
        if self.session.as_ref().is_some_and(Session::expires_soon) {
            self.refresh()?;
        }
        let Some(session) = &self.session else {
            return Ok(Changes::default());
        };
        let path = SyncState::path_for(session.user_id);
        let mut state = SyncState::load(&path);
        let remote = Supabase {
            client: &self.client,
            session,
        };
        let engine = Engine {
            store: &self.store,
            remote: &remote,
            user_id: session.user_id,
            welcome: &self.welcome,
        };
        let result = engine.run(&mut state);
        // Partial progress is still consistent: the cursor only moves on success.
        state.save(&path);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// An in-memory `entries` table with a server-side clock.
    #[derive(Default)]
    struct FakeServer {
        rows: RefCell<Vec<RemoteEntry>>,
        clock: RefCell<i64>,
    }

    impl Remote for FakeServer {
        fn fetch(&self, after: Option<PageKey>, limit: usize) -> CloudResult<Vec<RemoteEntry>> {
            let mut rows: Vec<RemoteEntry> = self
                .rows
                .borrow()
                .iter()
                .filter(|row| match after {
                    None => true,
                    Some(PageKey {
                        updated_at,
                        id: None,
                    }) => row.updated_at > updated_at,
                    Some(PageKey {
                        updated_at,
                        id: Some(id),
                    }) => (row.updated_at, row.id) > (updated_at, id),
                })
                .cloned()
                .collect();
            rows.sort_by_key(|row| (row.updated_at, row.id));
            Ok(rows.into_iter().take(limit).collect())
        }

        fn upsert(&self, upserts: &[EntryUpsert]) -> CloudResult<()> {
            let mut rows = self.rows.borrow_mut();
            // Postgres' now() is per transaction, so a batch shares one timestamp.
            *self.clock.borrow_mut() += 1000;
            let updated_at = DateTime::from_timestamp(*self.clock.borrow(), 0).unwrap();
            for upsert in upserts {
                let row = RemoteEntry {
                    id: upsert.id,
                    filename: upsert.filename.clone(),
                    content: upsert.content.clone(),
                    deleted: upsert.deleted,
                    updated_at,
                };
                rows.retain(|existing| existing.id != upsert.id);
                rows.push(row);
            }
            Ok(())
        }
    }

    struct Device {
        store: Store,
        state: SyncState,
    }

    impl Device {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!("freewrite-sync-{}", Uuid::new_v4()));
            Device {
                store: Store::open(dir).unwrap(),
                state: SyncState::default(),
            }
        }

        fn sync(&mut self, server: &FakeServer) -> Changes {
            let engine = Engine {
                store: &self.store,
                remote: server,
                user_id: Uuid::nil(),
                welcome: "Hi. Welcome to Freewrite :).",
            };
            engine.run(&mut self.state).unwrap()
        }

        fn contents(&self) -> Vec<String> {
            let mut contents: Vec<String> = self
                .store
                .list()
                .into_iter()
                .map(|(_, content)| content)
                .collect();
            contents.sort();
            contents
        }
    }

    impl Drop for Device {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(self.store.dir());
        }
    }

    fn new_entry(device: &Device, content: &str) -> String {
        let filename = storage::Entry::new().filename;
        device.store.write(&filename, content).unwrap();
        filename
    }

    #[test]
    fn entries_travel_between_devices() {
        let server = FakeServer::default();
        let (mut a, mut b) = (Device::new(), Device::new());
        let name = new_entry(&a, "\n\nfrom a");
        a.sync(&server);
        let changes = b.sync(&server);
        assert_eq!(changes.changed.len(), 1);
        assert_eq!(b.store.read(&name).unwrap(), "\n\nfrom a");

        b.store.write(&name, "\n\nfrom a, edited on b").unwrap();
        b.sync(&server);
        a.sync(&server);
        assert_eq!(a.store.read(&name).unwrap(), "\n\nfrom a, edited on b");

        // Syncing again changes nothing.
        assert!(a.sync(&server).is_empty());
        assert!(b.sync(&server).is_empty());
    }

    #[test]
    fn deletions_propagate() {
        let server = FakeServer::default();
        let (mut a, mut b) = (Device::new(), Device::new());
        let name = new_entry(&a, "\n\ndoomed");
        a.sync(&server);
        b.sync(&server);
        a.store.delete(&name).unwrap();
        a.sync(&server);
        let changes = b.sync(&server);
        assert_eq!(changes.removed.len(), 1);
        assert!(b.store.list().is_empty());
        assert!(server.rows.borrow()[0].deleted);
    }

    #[test]
    fn edits_survive_a_remote_deletion() {
        let server = FakeServer::default();
        let (mut a, mut b) = (Device::new(), Device::new());
        let name = new_entry(&a, "\n\nkeep me");
        a.sync(&server);
        b.sync(&server);
        a.store.delete(&name).unwrap();
        a.sync(&server);
        b.store.write(&name, "\n\nkeep me, please").unwrap();
        b.sync(&server);
        a.sync(&server);
        assert_eq!(a.store.read(&name).unwrap(), "\n\nkeep me, please");
    }

    #[test]
    fn conflicts_keep_both_versions() {
        let server = FakeServer::default();
        let (mut a, mut b) = (Device::new(), Device::new());
        let name = new_entry(&a, "\n\nbase");
        a.sync(&server);
        b.sync(&server);
        a.store.write(&name, "\n\nbase + a").unwrap();
        b.store.write(&name, "\n\nbase + b").unwrap();
        a.sync(&server);
        b.sync(&server);
        a.sync(&server);
        let expected = vec!["\n\nbase + a".to_string(), "\n\nbase + b".to_string()];
        assert_eq!(a.contents(), expected);
        assert_eq!(b.contents(), expected);
        // b kept its own version under the original name.
        assert_eq!(b.store.read(&name).unwrap(), "\n\nbase + b");
    }

    #[test]
    fn blank_pages_and_the_guide_stay_local() {
        let server = FakeServer::default();
        let mut a = Device::new();
        new_entry(&a, "\n\n");
        new_entry(&a, "\n\nHi. Welcome to Freewrite :).");
        a.sync(&server);
        assert!(server.rows.borrow().is_empty());
    }

    #[test]
    fn rejects_filenames_that_are_not_entries() {
        let server = FakeServer::default();
        let mut a = Device::new();
        server.rows.borrow_mut().push(RemoteEntry {
            id: Uuid::new_v4(),
            filename: "../../escape.md".into(),
            content: "nope".into(),
            deleted: false,
            updated_at: Utc::now(),
        });
        assert!(a.sync(&server).is_empty());
        assert!(a.store.list().is_empty());
    }

    #[test]
    fn a_missing_folder_is_not_a_mass_deletion() {
        let server = FakeServer::default();
        let mut a = Device::new();
        new_entry(&a, "\n\nprecious");
        a.sync(&server);
        fs::remove_dir_all(a.store.dir()).unwrap();
        let engine = Engine {
            store: &a.store,
            remote: &server,
            user_id: Uuid::nil(),
            welcome: "",
        };
        assert!(engine.run(&mut a.state).is_err());
        assert!(!server.rows.borrow()[0].deleted);
        fs::create_dir_all(a.store.dir()).unwrap();
    }

    /// Against a real (or local) Supabase project:
    /// `FREEWRITE_E2E_URL=... FREEWRITE_E2E_KEY=... cargo test -- --ignored`
    #[test]
    #[ignore]
    fn end_to_end_against_a_server() {
        let config = CloudConfig {
            url: std::env::var("FREEWRITE_E2E_URL").expect("FREEWRITE_E2E_URL"),
            anon_key: std::env::var("FREEWRITE_E2E_KEY").unwrap_or_else(|_| "test".into()),
        };
        let client = CloudClient::new(config);
        let email = format!("e2e-{}@example.com", Uuid::new_v4());
        let session_a = match client.sign_up(&email, "correct horse battery").unwrap() {
            SignUpOutcome::SignedIn(session) => session,
            SignUpOutcome::ConfirmEmail => panic!("turn off email confirmation for this test"),
        };
        assert!(client.sign_in(&email, "wrong password").is_err());
        let session_b = client.sign_in(&email, "correct horse battery").unwrap();
        let session_b = client.refresh(&session_b).unwrap();
        assert_eq!(session_a.user_id, session_b.user_id);

        let (a, b) = (Device::new(), Device::new());
        let run = |device: &mut Device, session: &Session| {
            let remote = Supabase {
                client: &client,
                session,
            };
            Engine {
                store: &device.store,
                remote: &remote,
                user_id: session.user_id,
                welcome: "",
            }
            .run(&mut device.state)
            .unwrap()
        };
        let (mut a, mut b) = (a, b);
        let names: Vec<String> = (0..(PAGE_SIZE + 5))
            .map(|i| new_entry(&a, &format!("\n\nentry {i}")))
            .collect();
        run(&mut a, &session_a);
        assert_eq!(run(&mut b, &session_b).changed.len(), PAGE_SIZE + 5);
        b.store.write(&names[0], "\n\nedited on b").unwrap();
        b.store.delete(&names[1]).unwrap();
        run(&mut b, &session_b);
        let changes = run(&mut a, &session_a);
        assert_eq!(a.store.read(&names[0]).unwrap(), "\n\nedited on b");
        assert!(a.store.read(&names[1]).is_err());
        assert_eq!((changes.changed.len(), changes.removed.len()), (1, 1));
        assert!(run(&mut a, &session_a).is_empty());
        client.sign_out(&session_a);
    }

    #[test]
    fn pages_through_large_accounts() {
        let server = FakeServer::default();
        let (mut a, mut b) = (Device::new(), Device::new());
        for i in 0..(PAGE_SIZE + 20) {
            new_entry(&a, &format!("\n\nentry {i}"));
        }
        a.sync(&server);
        b.sync(&server);
        assert_eq!(b.store.list().len(), PAGE_SIZE + 20);
    }
}
