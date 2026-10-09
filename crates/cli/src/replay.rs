//! `4s journal replay`: re-send a recorded session to a daemon and check that
//! it ends up the same, entry by entry (see docs/journal.md).
//!
//! The input is a recording from `4s journal export` (base project, entries,
//! final digest) or a raw journal file from `<data-dir>/journal/*.jsonl`
//! (`{"segment": {seq, time, base}}` lines followed by entries).

use crate::client::{Client, Seating};
use anyhow::{Context, Result, anyhow, bail};
use fours_protocol::*;
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;

/// Recorded methods that are not replayed: they depend on the engine host's
/// disk or real MIDI ports and never change the undoable state. (A
/// successful `project.load` starts a new segment and is not part of it, so
/// one inside a segment is a failed load.)
pub const NOT_REPLAYED: &[&str] =
    &["project.save", "project.load", "midi.connect", "midi.disconnect", "midi.rename", "midi.set_seat"];

/// The client name of the replay's own connection; its entries (the import,
/// controller page fixes) are left out of the comparison.
const REPLAY: &str = "replay";

pub struct Options<'a> {
    pub url: &'a str,
    pub token: Option<String>,
    pub file: &'a Path,
    /// Wait between entries as long as the recording did.
    pub realtime: bool,
    /// For a .jsonl journal file: which segment (0 = first; default last).
    pub segment: Option<usize>,
    /// Replay even if the daemon has unsaved changes (they are replaced).
    pub force: bool,
    /// On a divergence, rewrite the recording with what the replay did (for
    /// a deliberate behavior change; review it with `git diff`).
    pub accept: bool,
}

/// Read a recording, or a segment of a journal file (digest unknown).
pub fn load(file: &Path, segment: Option<usize>) -> Result<Recording> {
    let text = std::fs::read_to_string(file).with_context(|| format!("read {}", file.display()))?;
    if let Ok(r) = serde_json::from_str::<Recording>(&text) {
        return Ok(r);
    }
    // (segment start seq, recording). The request that started a segment is
    // written after its segment line but belongs before it: entries count
    // only if their seq is greater than the segment's.
    let mut segments: Vec<(u64, Recording)> = Vec::new();
    for (n, line) in text.lines().enumerate().filter(|(_, l)| !l.trim().is_empty()) {
        let v: Value = serde_json::from_str(line).with_context(|| format!("{}:{}: not JSON", file.display(), n + 1))?;
        if let Some(seg) = v.get("segment") {
            let base = serde_json::from_value(seg["base"].clone())
                .with_context(|| format!("{}:{}: bad segment base", file.display(), n + 1))?;
            let seq = seg["seq"].as_u64().unwrap_or(0);
            segments.push((
                seq,
                Recording { format_version: RECORDING_FORMAT_VERSION, base, entries: Vec::new(), digest: String::new() },
            ));
        } else {
            let entry: JournalEntry =
                serde_json::from_value(v).with_context(|| format!("{}:{}: not a journal entry", file.display(), n + 1))?;
            match segments.last_mut() {
                Some((start, s)) if entry.seq > *start => s.entries.push(entry),
                Some(_) => {}
                None => bail!("{}: entries before any segment line", file.display()),
            }
        }
    }
    let count = segments.len();
    if count == 0 {
        bail!("{}: neither a recording nor a journal file with a segment", file.display());
    }
    let i = segment.unwrap_or(count - 1);
    segments.into_iter().nth(i).map(|(_, r)| r).ok_or_else(|| anyhow!("{}: segment {i} of {count} (0-based)", file.display()))
}

fn short(changes: &[Change]) -> String {
    let s = serde_json::to_string(changes).unwrap_or_default();
    match s.char_indices().nth(400) {
        Some((i, _)) => format!("{}...", &s[..i]),
        None => s,
    }
}

/// A connection for `e`'s user and origin, in the seat it was recorded in.
/// A seat the client was put in at hello (auto-created for a user, so not
/// journaled) is not there yet: auto-seat as the client did.
async fn connect_seated(o: &Options<'_>, e: &JournalEntry) -> Result<Client> {
    let user = Some(e.user.clone());
    if let Some(seat) = &e.context.seat {
        let seating = Seating { user: user.clone(), seat: Some(seat.clone()), auto: false };
        if let Ok(c) = Client::connect(o.url, o.token.clone(), &e.origin, &seating).await {
            return Ok(c);
        }
    }
    let seating = Seating { user, seat: None, auto: e.context.seat.is_some() };
    Client::connect(o.url, o.token.clone(), &e.origin, &seating).await
}

pub async fn run(o: Options<'_>) -> Result<()> {
    let rec = load(o.file, o.segment)?;
    if o.accept && serde_json::from_str::<Recording>(&std::fs::read_to_string(o.file)?).is_err() {
        bail!("--accept rewrites a recording (.json from `4s journal export`), not a journal file");
    }
    let mut main = Client::connect(o.url, o.token.clone(), REPLAY, &Seating::unseated(Some(REPLAY.into()))).await?;
    let snap: Snapshot = serde_json::from_value(main.call(&Request::StateGet(Empty {})).await?)?;
    if snap.project.dirty && !o.force {
        bail!(
            "the daemon at {} has unsaved changes, and a replay replaces its project.\n\
             Replay into an isolated daemon (see docs/journal.md), or pass --force.",
            o.url
        );
    }
    main.call(&Request::ProjectImport(ProjectImportParams { file: rec.base.clone() })).await?;

    let expected: Vec<&JournalEntry> = rec
        .entries
        .iter()
        .filter(|e| !NOT_REPLAYED.contains(&e.method.as_str()) && e.origin != REPLAY)
        .collect();
    // One connection per recorded (user, origin, seat), so per-user undo,
    // held notes, and seat-relative requests behave as they did.
    let mut conns: HashMap<(String, String, Option<String>), Client> = HashMap::new();
    let mut last_time = expected.first().map(|e| e.time);
    for e in &expected {
        if o.realtime && let Some(t) = last_time {
            let wait = (e.time - t).clamp(0.0, 60.0);
            tokio::time::sleep(std::time::Duration::from_secs_f64(wait)).await;
        }
        last_time = Some(e.time);
        // Pads map columns to steps through the page, which follows the
        // playhead: put the page back where it was.
        if matches!(e.method.as_str(), "controller.press" | "controller.knob") {
            let c: ControllerState = serde_json::from_value(main.call(&Request::ControllerGet(Empty {})).await?)?;
            if c.page != e.context.page {
                // Keep `follow` as it is (choosing a page while playing
                // would otherwise turn it off).
                main.call(&Request::ControllerSetMode(ControllerModeParams {
                    page: Some(e.context.page),
                    follow: Some(c.follow),
                    ..Default::default()
                }))
                .await?;
            }
        }
        let req = parse_request(&e.method, Some(e.params.clone()))
            .map_err(|err| anyhow!("entry {} ({}): {err}", e.seq, e.method))?;
        let key = (e.user.clone(), e.origin.clone(), e.context.seat.clone());
        if !conns.contains_key(&key) {
            let c = connect_seated(&o, e).await?;
            conns.insert(key.clone(), c);
        }
        // Errors are compared below, with everything else.
        let _ = conns.get_mut(&key).unwrap().call(&req).await;
    }

    let got: Recording = serde_json::from_value(main.call(&Request::JournalExport(Empty {})).await?)?;
    drop(conns);
    let actual: Vec<&JournalEntry> = got.entries.iter().filter(|e| e.origin != REPLAY).collect();
    if let Err(divergence) = compare(&expected, &actual, &rec.digest, &got.digest) {
        if !o.accept {
            return Err(divergence);
        }
        let accepted = Recording {
            format_version: RECORDING_FORMAT_VERSION,
            base: rec.base.clone(),
            entries: actual.into_iter().cloned().collect(),
            digest: got.digest.clone(),
        };
        std::fs::write(o.file, serde_json::to_string_pretty(&accepted)? + "\n")?;
        println!("{divergence:#}");
        println!("accepted: rewrote {} with the replayed result; review it with `git diff`", o.file.display());
        return Ok(());
    }
    let skipped = rec.entries.len() - expected.len();
    println!(
        "replay matched: {} entries{}, digest {}",
        expected.len(),
        if skipped > 0 { format!(" ({skipped} not replayed: {})", NOT_REPLAYED.join(", ")) } else { String::new() },
        got.digest
    );
    Ok(())
}

/// Entry by entry (method, changes, error), then the final digest (if the
/// recording has one).
fn compare(expected: &[&JournalEntry], actual: &[&JournalEntry], want_digest: &str, got_digest: &str) -> Result<()> {
    for (i, (want, have)) in expected.iter().zip(actual).enumerate() {
        if want.method != have.method || want.changes != have.changes || want.error != have.error {
            bail!(
                "replay diverged at entry {} of {} (recorded seq {}: {} by {} from {}):\n  \
                 recorded: changes {} error {:?}\n  replayed: {} changes {} error {:?}",
                i + 1,
                expected.len(),
                want.seq,
                want.method,
                want.user,
                want.origin,
                short(&want.changes),
                want.error,
                have.method,
                short(&have.changes),
                have.error
            );
        }
    }
    if expected.len() != actual.len() {
        bail!("replay diverged: {} entries recorded, {} replayed", expected.len(), actual.len());
    }
    if !want_digest.is_empty() && want_digest != got_digest {
        bail!("replay diverged: final state digest {want_digest} recorded, {got_digest} replayed");
    }
    Ok(())
}

/// The recording as a shell script of `4s call` lines: readable and
/// editable, but approximate (each line is its own connection, so held
/// notes end at once, and there is no comparison).
pub fn to_script(rec: &Recording) -> String {
    let q = |s: &str| format!("'{}'", s.replace('\'', r"'\''"));
    let mut out = String::from(
        "#!/usr/bin/env bash\n\
         # A 4S session as `4s call` lines (from `4s journal export --format sh`).\n\
         # Approximate; for an exact, checked replay use `4s journal replay <recording.json>`.\n\
         set -e\n",
    );
    let base = serde_json::json!({ "file": rec.base });
    out.push_str(&format!("4s call project.import {}\n", q(&base.to_string())));
    for e in &rec.entries {
        let skip = if NOT_REPLAYED.contains(&e.method.as_str()) { "# " } else { "" };
        out.push_str(&format!("{skip}4s --user {} call {} {}\n", q(&e.user), e.method, q(&e.params.to_string())));
    }
    out
}
