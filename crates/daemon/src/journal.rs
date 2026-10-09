//! The journal and undo history.
//!
//! The journal is an append-only record of every request that could change
//! state: who sent it, when, the request itself, and the changes it made as
//! (key, before, after) over the project's addressable state (see
//! `Core::doc`). It is kept in memory for queries and appended to JSONL files
//! under `<data_dir>/journal/` by a writer thread, off the core lock.
//!
//! Undo is per user and selective: each user has their own undo and redo
//! stacks, and undoing reverts only that user's changes. A key that another
//! user wrote since is left alone and reported as skipped. Undo and redo are
//! journal entries themselves (`reverts` names the entry they revert), so the
//! journal is never rewritten.

use fours_protocol::{Change, HistoryInfo, JournalEntry, JournalGetParams, ProjectFile};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc;

/// The undoable state as flat keys (see `Core::doc`). Params and steps at
/// their defaults are left out, so `null` means "default" for them.
pub type Doc = BTreeMap<String, Value>;

/// Keys whose values differ between two docs, sorted by key.
pub fn diff(before: &Doc, after: &Doc) -> Vec<Change> {
    let mut out: Vec<Change> = before
        .iter()
        .filter(|(k, b)| after.get(*k) != Some(*b))
        .map(|(k, b)| Change { key: k.clone(), before: b.clone(), after: after.get(k).cloned().unwrap_or(Value::Null) })
        .collect();
    out.extend(
        after
            .iter()
            .filter(|(k, _)| !before.contains_key(*k))
            .map(|(k, a)| Change { key: k.clone(), before: Value::Null, after: a.clone() }),
    );
    out.sort_by(|a, b| a.key.cmp(&b.key));
    out
}

/// A stable hash of a doc (FNV-1a over its JSON, keys sorted), to check that
/// a replay ends in the same state.
pub fn digest(doc: &Doc) -> String {
    let json = serde_json::to_string(doc).unwrap_or_default();
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in json.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}")
}

/// The instrument id a key belongs to: `instrument:bass`, `param:bass.cutoff`,
/// `step:drums.kick.3`, `note:bass.2`, `route:drums.kick`. (For
/// `param:mixer.1.volume` this is `mixer`, a reserved id no instrument has.)
pub fn owner(key: &str) -> Option<&str> {
    let (kind, rest) = key.split_once(':')?;
    match kind {
        "instrument" | "param" | "step" | "note" | "route" => rest.split('.').next(),
        _ => None,
    }
}

/// A short description of a request for undo menus: the method and the
/// params that name what it touched, else the first changed key.
pub fn label(method: &str, params: &Value, changes: &[Change]) -> String {
    let mut parts = vec![method.to_string()];
    for k in ["type", "path", "id", "source", "instrument", "voice", "step", "n"] {
        match params.get(k) {
            Some(Value::String(s)) => parts.push(s.clone()),
            Some(Value::Number(n)) => parts.push(n.to_string()),
            _ => {}
        }
    }
    if parts.len() == 1
        && let Some(c) = changes.first()
    {
        parts.push(c.key.split_once(':').map_or(c.key.as_str(), |(_, rest)| rest).to_string());
    }
    parts.join(" ")
}

// ---- undo history -----------------------------------------------------------

/// Undo steps kept per user.
const MAX_STEPS: usize = 200;

struct Step {
    /// The journal entry this step was last recorded from.
    seq: u64,
    label: String,
    changes: Vec<Change>,
    /// A params-only edit that a following edit of the same params may
    /// merge into (one step for a whole knob drag).
    mergeable: bool,
}

#[derive(Default)]
struct Stacks {
    undo: Vec<Step>,
    redo: Vec<Step>,
}

/// What an undo or redo would set, after leaving out conflicting keys.
pub struct Plan {
    /// The journal entry being reverted.
    pub seq: u64,
    pub label: String,
    /// Keys and the values to set them to.
    pub sets: Vec<(String, Value)>,
    /// Keys left alone because another user changed them since.
    pub skipped: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Summary {
    pub undo_label: Option<String>,
    pub redo_label: Option<String>,
    pub undo_count: u32,
    pub redo_count: u32,
}

#[derive(Default)]
pub struct History {
    users: HashMap<String, Stacks>,
    /// The last user to write each key (by an edit, undo, or redo).
    last_touch: HashMap<String, String>,
}

impl History {
    /// Record an edit. Consecutive edits of the same params merge into one
    /// step (unless another user changed one of them in between); any other
    /// edit starts a new one and clears the user's redo.
    pub fn record(&mut self, user: &str, seq: u64, label: String, changes: &[Change]) {
        let interleaved = changes.iter().any(|c| self.last_touch.get(&c.key).is_some_and(|u| u != user));
        self.touch(user, changes);
        let st = self.users.entry(user.to_string()).or_default();
        st.redo.clear();
        let params_only = changes.iter().all(|c| c.key.starts_with("param:"));
        if params_only
            && !interleaved
            && let Some(top) = st.undo.last_mut()
            && top.mergeable
            && top.changes.len() == changes.len()
            && top.changes.iter().zip(changes).all(|(a, b)| a.key == b.key)
        {
            for (t, c) in top.changes.iter_mut().zip(changes) {
                t.after = c.after.clone();
            }
            top.seq = seq;
            return;
        }
        st.undo.push(Step { seq, label, changes: changes.to_vec(), mergeable: params_only });
        if st.undo.len() > MAX_STEPS {
            st.undo.remove(0);
        }
    }

    /// What undoing (or redoing) the user's top step would set. Applying a
    /// step sets each key back to its `before`. A key another user wrote
    /// last is skipped. Creating or removing an instrument is skipped if
    /// another user wrote its key, or (when removing) anything it owns; the
    /// keys it owns are then skipped too.
    pub fn plan(&self, user: &str, redo: bool) -> Option<Plan> {
        let st = self.users.get(user)?;
        let step = if redo { st.redo.last()? } else { st.undo.last()? };
        let by_other = |k: &str| self.last_touch.get(k).is_some_and(|u| u != user);
        let mut blocked: HashSet<&str> = HashSet::new();
        for c in &step.changes {
            if let Some(id) = c.key.strip_prefix("instrument:") {
                let removing = c.before.is_null();
                let owned_by_other =
                    || self.last_touch.iter().any(|(k, u)| u != user && owner(k) == Some(id) && k != &c.key);
                if by_other(&c.key) || (removing && owned_by_other()) {
                    blocked.insert(id);
                }
            }
        }
        let (mut sets, mut skipped) = (Vec::new(), Vec::new());
        for c in &step.changes {
            if by_other(&c.key) || owner(&c.key).is_some_and(|id| blocked.contains(id)) {
                skipped.push(c.key.clone());
            } else {
                sets.push((c.key.clone(), c.before.clone()));
            }
        }
        Some(Plan { seq: step.seq, label: step.label.clone(), sets, skipped })
    }

    /// Finish an undo (or redo): the step moves to the other stack as the
    /// changes actually made, so redoing restores exactly what undo
    /// replaced. A step that changed nothing is dropped.
    pub fn finish(&mut self, user: &str, redo: bool, seq: u64, changes: Vec<Change>) {
        self.touch(user, &changes);
        let st = self.users.entry(user.to_string()).or_default();
        let Some(step) = (if redo { st.redo.pop() } else { st.undo.pop() }) else { return };
        if let Some(top) = st.undo.last_mut() {
            top.mergeable = false;
        }
        if changes.is_empty() {
            return;
        }
        let next = Step { seq, label: step.label, changes, mergeable: false };
        if redo { st.undo.push(next) } else { st.redo.push(next) }
    }

    /// Note `user` as the last writer of each changed key. A removed
    /// instrument's keys are forgotten, so whoever edited the old one does
    /// not block undoing a new instrument with the same id.
    fn touch(&mut self, user: &str, changes: &[Change]) {
        let removed: Vec<&str> = changes
            .iter()
            .filter(|c| c.after.is_null())
            .filter_map(|c| c.key.strip_prefix("instrument:"))
            .collect();
        self.last_touch.retain(|k, _| !owner(k).is_some_and(|id| removed.contains(&id)));
        for c in changes {
            let gone_with_it = !c.key.starts_with("instrument:") && owner(&c.key).is_some_and(|id| removed.contains(&id));
            if !gone_with_it {
                self.last_touch.insert(c.key.clone(), user.to_string());
            }
        }
    }

    /// Forget all history (a project was loaded or started). Returns the
    /// users who had any.
    pub fn clear(&mut self) -> Vec<String> {
        self.last_touch.clear();
        self.users.drain().filter(|(_, s)| !s.undo.is_empty() || !s.redo.is_empty()).map(|(u, _)| u).collect()
    }

    pub fn summary(&self, user: &str) -> Summary {
        let st = self.users.get(user);
        let undo = st.map(|s| &s.undo[..]).unwrap_or_default();
        let redo = st.map(|s| &s.redo[..]).unwrap_or_default();
        Summary {
            undo_label: undo.last().map(|s| s.label.clone()),
            redo_label: redo.last().map(|s| s.label.clone()),
            undo_count: undo.len() as u32,
            redo_count: redo.len() as u32,
        }
    }

    pub fn info(&self, user: &str) -> HistoryInfo {
        let labels = |v: Option<&Vec<Step>>| -> Vec<String> {
            v.map(|v| v.iter().rev().take(20).map(|s| s.label.clone()).collect()).unwrap_or_default()
        };
        let st = self.users.get(user);
        HistoryInfo { user: user.to_string(), undo: labels(st.map(|s| &s.undo)), redo: labels(st.map(|s| &s.redo)) }
    }
}

// ---- journal storage ----------------------------------------------------------

/// Entries kept in memory for `journal.get`.
const RING: usize = 10_000;
/// Rotate journal files at this size.
const FILE_BYTES: u64 = 50 * 1024 * 1024;
/// Journal files kept on disk.
const FILES_KEPT: usize = 20;

pub struct Journal {
    ring: VecDeque<JournalEntry>,
    seq: u64,
    file: Option<mpsc::Sender<String>>,
}

impl Journal {
    /// A journal that also appends to files in `dir`, if given.
    pub fn new(dir: Option<PathBuf>) -> Self {
        let file = dir.and_then(|dir| {
            let (tx, rx) = mpsc::channel::<String>();
            std::thread::Builder::new()
                .name("4s-journal".into())
                .spawn(move || write_files(&dir, rx))
                .map_err(|e| tracing::warn!("journal file writer not started: {e}"))
                .ok()
                .map(|_| tx)
        });
        Self { ring: VecDeque::new(), seq: 0, file }
    }

    pub fn next_seq(&mut self) -> u64 {
        self.seq += 1;
        self.seq
    }

    /// The last seq handed out.
    pub fn seq(&self) -> u64 {
        self.seq
    }

    /// Mark the start of a history segment in the journal file: a
    /// `{"segment": {seq, time, base}}` line with the project at that point,
    /// so a file can be replayed from it (entries after it have greater seqs).
    pub fn push_segment(&mut self, base: &ProjectFile, time: f64) {
        if let Some(tx) = &self.file {
            let line = serde_json::json!({ "segment": { "seq": self.seq, "time": time, "base": base } });
            let _ = tx.send(line.to_string());
        }
    }

    /// Every entry after `seq`, oldest first; None if some have already been
    /// dropped from memory.
    pub fn since(&self, seq: u64) -> Option<Vec<JournalEntry>> {
        let first = self.ring.front().map_or(self.seq + 1, |e| e.seq);
        if first > seq + 1 {
            return None;
        }
        Some(self.ring.iter().filter(|e| e.seq > seq).cloned().collect())
    }

    pub fn push(&mut self, entry: JournalEntry) {
        if let Some(tx) = &self.file
            && let Ok(line) = serde_json::to_string(&entry)
        {
            let _ = tx.send(line);
        }
        if self.ring.len() == RING {
            self.ring.pop_front();
        }
        self.ring.push_back(entry);
    }

    /// The newest entries matching the query, oldest first.
    pub fn get(&self, p: &JournalGetParams) -> Vec<JournalEntry> {
        let limit = p.limit.unwrap_or(100) as usize;
        let mut out: Vec<JournalEntry> = self
            .ring
            .iter()
            .rev()
            .filter(|e| p.since.is_none_or(|s| e.seq > s))
            .filter(|e| p.user.as_ref().is_none_or(|u| &e.user == u))
            .take(limit)
            .cloned()
            .collect();
        out.reverse();
        out
    }
}

/// The file writer thread: one JSON entry per line, flushed per entry so a
/// crash keeps everything before it.
fn write_files(dir: &Path, rx: mpsc::Receiver<String>) {
    if let Err(e) = std::fs::create_dir_all(dir) {
        tracing::warn!("journal dir {}: {e}", dir.display());
        return;
    }
    let started = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    let pid = std::process::id();
    let mut part = 0;
    let open = |part: u32| {
        prune(dir);
        let path = dir.join(format!("{started}-{pid}-{part}.jsonl"));
        std::fs::OpenOptions::new().create(true).append(true).open(&path).map_err(|e| {
            tracing::warn!("journal file {}: {e}", path.display());
        })
    };
    let Ok(mut file) = open(part) else { return };
    let mut written = 0u64;
    for line in rx {
        if writeln!(file, "{line}").and_then(|_| file.flush()).is_err() {
            continue;
        }
        written += line.len() as u64 + 1;
        if written >= FILE_BYTES {
            part += 1;
            written = 0;
            match open(part) {
                Ok(f) => file = f,
                Err(()) => return,
            }
        }
    }
}

/// Keep the newest `FILES_KEPT - 1` journal files (one is about to be opened).
fn prune(dir: &Path) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = rd
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("jsonl"))
        .map(|p| (std::fs::metadata(&p).and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH), p))
        .collect();
    files.sort();
    let excess = files.len().saturating_sub(FILES_KEPT - 1);
    for (_, p) in files.into_iter().take(excess) {
        let _ = std::fs::remove_file(p);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn doc(pairs: &[(&str, Value)]) -> Doc {
        pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
    }

    fn set(h: &mut History, user: &str, seq: u64, before: &Doc, after: &Doc) {
        h.record(user, seq, format!("edit {seq}"), &diff(before, after));
    }

    #[test]
    fn diff_reports_only_changed_keys_including_added_and_removed() {
        let a = doc(&[("param:x", json!(1.0)), ("param:y", json!(2.0)), ("step:d.kick.0", json!(1))]);
        let b = doc(&[("param:x", json!(1.0)), ("param:y", json!(3.0)), ("route:d", json!(1))]);
        let keys: Vec<_> = diff(&a, &b).into_iter().map(|c| (c.key, c.before, c.after)).collect();
        assert_eq!(
            keys,
            vec![
                ("param:y".into(), json!(2.0), json!(3.0)),
                ("route:d".into(), Value::Null, json!(1)),
                ("step:d.kick.0".into(), json!(1), Value::Null),
            ]
        );
    }

    #[test]
    fn a_knob_drag_is_one_step_and_undo_breaks_the_merge() {
        let mut h = History::default();
        let v = |x: f64| doc(&[("param:mixer.1.volume", json!(x))]);
        set(&mut h, "a", 1, &v(1.0), &v(0.9));
        set(&mut h, "a", 2, &v(0.9), &v(0.8));
        set(&mut h, "a", 3, &v(0.8), &v(0.7));
        let p = h.plan("a", false).unwrap();
        assert_eq!((p.seq, p.sets), (3, vec![("param:mixer.1.volume".into(), json!(1.0))]));
        assert_eq!(h.summary("a").undo_count, 1);

        // Undoing a later edit exposes the drag again; editing the same knob
        // after that is a new step, not part of the undone-to drag.
        let pan = |x: f64| doc(&[("param:mixer.1.pan", json!(x))]);
        set(&mut h, "a", 4, &pan(0.0), &pan(0.5));
        h.finish("a", false, 5, diff(&pan(0.5), &pan(0.0)));
        set(&mut h, "a", 6, &v(0.7), &v(0.3));
        assert_eq!(h.summary("a").undo_count, 2);
        assert_eq!(h.plan("a", false).unwrap().sets, vec![("param:mixer.1.volume".into(), json!(0.7))]);
    }

    #[test]
    fn another_users_edit_in_between_breaks_a_drag() {
        let mut h = History::default();
        let v = |x: f64| doc(&[("param:mixer.1.volume", json!(x))]);
        set(&mut h, "alice", 1, &v(1.0), &v(0.9));
        set(&mut h, "bob", 2, &v(0.9), &v(0.5));
        set(&mut h, "alice", 3, &v(0.5), &v(0.4));
        // Alice's undo returns to bob's value, not past it.
        assert_eq!(h.plan("alice", false).unwrap().sets, vec![("param:mixer.1.volume".into(), json!(0.5))]);
        assert_eq!(h.summary("alice").undo_count, 2);
    }

    #[test]
    fn a_removed_instruments_old_edits_do_not_block_a_new_one() {
        let mut h = History::default();
        let bass = |cutoff: Option<f64>| {
            let mut d = doc(&[("instrument:bass", json!({"type": "tb303", "name": "Bass"}))]);
            if let Some(c) = cutoff {
                d.insert("param:bass.cutoff".into(), json!(c));
            }
            d
        };
        set(&mut h, "bob", 1, &Doc::new(), &bass(None));
        set(&mut h, "bob", 2, &bass(None), &bass(Some(0.2)));
        set(&mut h, "bob", 3, &bass(Some(0.2)), &Doc::new());
        set(&mut h, "alice", 4, &Doc::new(), &bass(None));
        let p = h.plan("alice", false).unwrap();
        assert!(p.skipped.is_empty());
        assert_eq!(p.sets, vec![("instrument:bass".to_string(), Value::Null)]);
    }

    #[test]
    fn undo_skips_keys_another_user_changed_and_redo_restores_what_undo_replaced() {
        let mut h = History::default();
        let d = |pan: f64, vol: f64| doc(&[("param:mixer.1.pan", json!(pan)), ("param:mixer.1.volume", json!(vol))]);
        set(&mut h, "alice", 1, &d(0.0, 1.0), &d(0.5, 0.5));
        let bob = doc(&[("param:mixer.1.pan", json!(-0.5))]);
        h.record("bob", 2, "bob".into(), &diff(&doc(&[("param:mixer.1.pan", json!(0.5))]), &bob));

        let p = h.plan("alice", false).unwrap();
        assert_eq!(p.skipped, vec!["param:mixer.1.pan".to_string()]);
        assert_eq!(p.sets, vec![("param:mixer.1.volume".to_string(), json!(1.0))]);

        // The undo set volume back to 1.0; redo sets it to 0.5 again.
        h.finish("alice", false, 3, diff(&d(-0.5, 0.5), &d(-0.5, 1.0)));
        let r = h.plan("alice", true).unwrap();
        assert_eq!((r.seq, r.sets), (3, vec![("param:mixer.1.volume".to_string(), json!(0.5))]));
        // Bob's own undo is unaffected by alice's.
        assert_eq!(h.plan("bob", false).unwrap().sets, vec![("param:mixer.1.pan".to_string(), json!(0.5))]);
    }

    #[test]
    fn undoing_an_add_is_blocked_by_another_users_edit_of_the_instrument() {
        let mut h = History::default();
        let added = doc(&[("instrument:bass", json!({"type": "tb303", "name": "Bass"})), ("route:bass", json!(2))]);
        set(&mut h, "alice", 1, &Doc::new(), &added);
        h.record("bob", 2, "bob".into(), &diff(&Doc::new(), &doc(&[("note:bass.0", json!({"note": 36}))])));
        let p = h.plan("alice", false).unwrap();
        assert!(p.sets.is_empty());
        assert_eq!(p.skipped, vec!["instrument:bass".to_string(), "route:bass".to_string()]);
    }

    #[test]
    fn undoing_a_remove_restores_everything_the_instrument_owned() {
        let mut h = History::default();
        let before = doc(&[
            ("channel:2", json!("Bass")),
            ("instrument:bass", json!({"type": "tb303", "name": "Bass"})),
            ("note:bass.0", json!({"note": 36, "accent": false, "slide": false})),
            ("param:bass.cutoff", json!(0.3)),
            ("route:bass", json!(2)),
        ]);
        set(&mut h, "alice", 1, &before, &Doc::new());
        let p = h.plan("alice", false).unwrap();
        assert!(p.skipped.is_empty());
        assert_eq!(p.sets.len(), 5);
        assert!(p.sets.iter().all(|(k, v)| before[k] == *v));
    }

    #[test]
    fn clearing_forgets_stacks_and_reports_who_had_history() {
        let mut h = History::default();
        set(&mut h, "a", 1, &Doc::new(), &doc(&[("param:x", json!(1))]));
        assert_eq!(h.clear(), vec!["a".to_string()]);
        assert!(h.plan("a", false).is_none());
    }
}
