//! Filesystem hints for the log readers and the small, per-account usage caches.

use notify::{event::{CreateKind, ModifyKind, RemoveKind}, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub(crate) const RECONCILE: Duration = Duration::from_secs(120);
pub(crate) const RETRY_WATCH: Duration = Duration::from_secs(15);
const MAX_PENDING: usize = 512;

#[derive(Default)]
pub(crate) struct Interests {
    pub logs: Vec<PathBuf>,
    pub usage: HashSet<PathBuf>,
}

#[derive(Default)]
pub(crate) struct Changes {
    pub paths: HashMap<PathBuf, bool>,
    pub directories: HashSet<PathBuf>,
    pub rescan: bool,
}

impl Changes {
    fn is_empty(&self) -> bool {
        !self.rescan && self.paths.is_empty() && self.directories.is_empty()
    }

    pub fn needs_scan(&self, roots: &[PathBuf]) -> bool {
        self.rescan || self.directories.iter().any(|dir| {
            roots.iter().any(|root| dir.starts_with(root) || root.starts_with(dir))
        })
    }

    pub fn log_paths(&self, roots: &[PathBuf]) -> Vec<(&Path, bool)> {
        self.paths.iter()
            .filter(|(path, _)| is_log(path) && roots.iter().any(|root| path.starts_with(root)))
            .map(|(path, replaced)| (path.as_path(), *replaced)).collect()
    }

    fn record(&mut self, event: notify::Result<Event>, interests: &Interests) {
        if self.rescan {
            return;
        }
        let Ok(event) = event else {
            self.request_rescan();
            return;
        };
        if event.need_rescan() {
            self.request_rescan();
            return;
        }
        if matches!(event.kind, EventKind::Access(_)) {
            return;
        }
        let replacement = matches!(event.kind,
            EventKind::Create(_) | EventKind::Remove(_) | EventKind::Modify(ModifyKind::Name(_)));
        let directory_change = matches!(event.kind,
            EventKind::Any | EventKind::Create(CreateKind::Any | CreateKind::Folder)
                | EventKind::Remove(RemoveKind::Any | RemoveKind::Folder)
                | EventKind::Modify(ModifyKind::Name(_)));
        if event.paths.is_empty() {
            self.request_rescan();
            return;
        }
        for path in event.paths {
            let under_logs = interests.logs.iter().any(|root| path.starts_with(root));
            if interests.usage.contains(&path) || under_logs && is_log(&path) {
                *self.paths.entry(path).or_default() |= replacement;
            } else if directory_change && (under_logs
                || interests.logs.iter().any(|root| root.starts_with(&path))
                || interests.usage.iter().any(|file| file.starts_with(&path)))
            {
                self.directories.insert(path);
            }
            if self.paths.len() + self.directories.len() > MAX_PENDING {
                self.request_rescan();
                break;
            }
        }
    }

    fn request_rescan(&mut self) {
        *self = Self { rescan: true, ..Self::default() };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn native_events_cover_a_new_root_and_nested_log_writes() {
        let temp = tempfile::tempdir().unwrap();
        let root = watch_path(temp.path().join("sessions"));
        let interests = || Interests { logs: vec![root.clone()], ..Interests::default() };
        let (tx, rx) = std::sync::mpsc::channel();
        let mut watch = LogWatch::new(move || { let _ = tx.send(()); });
        assert!(watch.sync(interests(), false));

        let wait_for = |watch: &LogWatch, accept: &dyn Fn(Changes) -> bool| {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            loop {
                rx.recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
                    .expect("expected native filesystem change");
                if accept(watch.take()) { break; }
            }
        };
        let session = root.join("2026").join("session.jsonl");
        std::fs::create_dir_all(session.parent().unwrap()).unwrap();
        wait_for(&watch, &|changes| changes.needs_scan(&[root.clone()]));
        assert!(watch.sync(interests(), false));
        std::fs::write(&session, b"first\n").unwrap();
        wait_for(&watch, &|changes| changes.paths.contains_key(&session));
        let mut log = std::fs::OpenOptions::new().append(true).open(&session).unwrap();
        log.write_all(b"second\n").unwrap();
        drop(log);
        wait_for(&watch, &|changes| changes.paths.contains_key(&session));
    }

    #[test]
    fn irrelevant_files_do_not_wake_the_reader() {
        let root = PathBuf::from("data");
        let usage = root.join("usage.json");
        let interests = Interests { logs: vec![], usage: HashSet::from([usage.clone()]) };
        let mut changes = Changes::default();
        changes.record(Ok(Event::new(EventKind::Create(CreateKind::Any)).add_path(root.join("state.json"))), &interests);
        assert!(changes.is_empty());
        changes.record(Ok(Event::new(EventKind::Create(CreateKind::Any)).add_path(usage.clone())), &interests);
        assert!(changes.paths.contains_key(&usage));
        changes.record(Err(notify::Error::generic("watch overflow")), &interests);
        assert!(changes.rescan);
    }
}

fn is_log(path: &Path) -> bool {
    path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("jsonl"))
}

#[derive(Default)]
struct Pending {
    interests: Interests,
    changes: Changes,
}

pub(crate) struct LogWatch {
    pending: Arc<Mutex<Pending>>,
    wake: Arc<dyn Fn() + Send + Sync>,
    backend: Option<RecommendedWatcher>,
    watched: BTreeMap<PathBuf, RecursiveMode>,
}

impl LogWatch {
    pub fn new(wake: impl Fn() + Send + Sync + 'static) -> Self {
        Self {
            pending: Arc::new(Mutex::new(Pending::default())),
            wake: Arc::new(wake),
            backend: None,
            watched: BTreeMap::new(),
        }
    }

    pub fn take(&self) -> Changes {
        std::mem::take(&mut self.pending.lock().unwrap().changes)
    }

    /// Watch existing log roots recursively and existing ancestors of missing roots
    /// nonrecursively, so that creating a profile's first log directory wakes us up.
    pub fn sync(&mut self, interests: Interests, restart: bool) -> bool {
        let mut desired = BTreeMap::new();
        for root in &interests.logs {
            if root.is_dir() {
                desired.insert(root.clone(), RecursiveMode::Recursive);
            }
            if let Some(parent) = root.ancestors().skip(1).find(|p| p.is_dir()) {
                desired.entry(parent.to_path_buf()).or_insert(RecursiveMode::NonRecursive);
            }
        }
        for path in &interests.usage {
            if let Some(parent) = path.ancestors().skip(1).find(|p| p.is_dir()) {
                desired.entry(parent.to_path_buf()).or_insert(RecursiveMode::NonRecursive);
            }
        }
        self.pending.lock().unwrap().interests = interests;
        if restart || desired.is_empty() {
            self.backend = None;
            self.watched.clear();
        }
        if desired.is_empty() {
            return true;
        }
        if self.backend.is_none() {
            let pending = Arc::clone(&self.pending);
            let wake = Arc::clone(&self.wake);
            self.backend = notify::recommended_watcher(move |event| {
                let mut pending = pending.lock().unwrap();
                let was_empty = pending.changes.is_empty();
                let Pending { interests, changes } = &mut *pending;
                changes.record(event, interests);
                if was_empty && !changes.is_empty() {
                    wake();
                }
            }).ok();
        }
        let Some(backend) = &mut self.backend else { return false };
        self.watched.retain(|path, mode| {
            if desired.get(path) != Some(mode) {
                let _ = backend.unwatch(path);
                return false;
            }
            true
        });
        // A recursive watch may cover another planned path; don't unwatch a
        // successful child registration when adding an unrelated account.
        for (path, mode) in &desired {
            if !self.watched.contains_key(path) && backend.watch(path, *mode).is_ok() {
                self.watched.insert(path.clone(), *mode);
            }
        }
        self.watched == desired
    }
}

/// Use the canonical spelling of the existing part of a path on both readers
/// and the watcher. The suffix may not exist until the provider writes its first log.
pub(crate) fn watch_path(path: PathBuf) -> PathBuf {
    for ancestor in path.ancestors() {
        if let Ok(resolved) = ancestor.canonicalize() {
            let suffix = path.strip_prefix(ancestor).unwrap();
            return if suffix.as_os_str().is_empty() { resolved } else { resolved.join(suffix) };
        }
    }
    path
}
