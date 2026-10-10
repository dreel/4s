//! Project file format (`<name>.4s/project.json`). See docs/project-format.md.

use crate::clip::Placement;
use crate::types::*;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use ts_rs::TS;

/// Current project format version. Bump and add a migration when the format
/// changes incompatibly.
pub const PROJECT_FORMAT_VERSION: u32 = 5;
/// Oldest version this build can load. Version 1 (the fixed 8-track kit)
/// was dropped with RFC 0004, without a migration.
pub const OLDEST_PROJECT_FORMAT_VERSION: u32 = 2;
pub const PROJECT_FILE_NAME: &str = "project.json";
pub const PROJECT_EXTENSION: &str = "4s";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
pub struct ProjectFile {
    pub format_version: u32,
    /// In creation order.
    pub instruments: Vec<ProjectInstrument>,
    /// In display order. `n` is the channel number in `mixer.<n>.*` paths.
    pub channels: Vec<ChannelInfo>,
    /// Source (`drums`, `drums.kick`) -> channel number.
    pub routes: BTreeMap<String, u32>,
    /// Parameter values by path. Unknown paths are ignored on load; missing
    /// paths take their defaults.
    pub params: BTreeMap<String, f64>,
    /// Per instrument id (RFC 0008): its clip pool, selected clip, and
    /// arrangement. An instrument left out has one empty clip.
    #[serde(default)]
    pub tracks: BTreeMap<String, ProjectTrack>,
    pub controller: ProjectController,
    /// Performer setups by seat name (RFC 0007): focus, knob page, note
    /// bindings, CC maps.
    #[serde(default)]
    pub seats: BTreeMap<String, SeatConfig>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
pub struct ProjectInstrument {
    pub id: String,
    #[serde(rename = "type")]
    #[ts(rename = "type")]
    pub kind: InstrumentType,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
#[serde(untagged)]
pub enum ProjectPattern {
    Notes(String),
    Drums(BTreeMap<Voice, String>),
}

/// An instrument's track in a project file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
pub struct ProjectTrack {
    /// The selected clip's id.
    pub selected: u32,
    /// The pool by clip id.
    pub clips: BTreeMap<u32, ProjectClip>,
    /// Placements on the song timeline, by start.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub arrangement: Vec<Placement>,
}

/// A clip in a project file: a step pattern when one says exactly the same
/// thing (drum step strings per voice, `{"kick": "x---x---"}`, or a note
/// string, `"C2 C2! D#2~ -"`), else events in text form (`tick:note:len:vel`,
/// see `format_events`); both absent for an empty clip.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
pub struct ProjectClip {
    /// Default: the clip's id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Ticks; absent follows `sequencer.length`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub length: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern: Option<ProjectPattern>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub events: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
pub struct ProjectController {
    /// The grid page follows the playhead.
    pub follow: bool,
}

/// A migration upgrades a project from version N to N+1.
/// `MIGRATIONS[0]` upgrades `OLDEST_PROJECT_FORMAT_VERSION` to the next one,
/// and so on.
type Migration = fn(Value) -> Result<Value, String>;
const MIGRATIONS: &[Migration] = &[v2_to_v3, v3_to_v4, v4_to_v5];

/// v5 (RFC 0008 phase B): each instrument has a track with a pool of clips.
/// An instrument's pattern or clip becomes clip 1 of its pool, selected.
fn v4_to_v5(mut v: Value) -> Result<Value, String> {
    let mut tracks = serde_json::Map::new();
    let take = |v: &mut Value, k: &str| v.as_object_mut().and_then(|o| o.remove(k));
    if let Some(Value::Object(patterns)) = take(&mut v, "patterns") {
        for (id, p) in patterns {
            tracks.insert(id, serde_json::json!({ "selected": 1, "clips": { "1": { "pattern": p } } }));
        }
    }
    if let Some(Value::Object(clips)) = take(&mut v, "clips") {
        for (id, c) in clips {
            tracks.insert(id, serde_json::json!({ "selected": 1, "clips": { "1": c } }));
        }
    }
    v["tracks"] = Value::Object(tracks);
    v["format_version"] = 5.into();
    Ok(v)
}

/// v4 (RFC 0007 phase 2): sequences are clips. Step patterns still load as
/// they are; clips that do not fit them are saved under `clips`.
fn v3_to_v4(mut v: Value) -> Result<Value, String> {
    v["clips"] = Value::Object(Default::default());
    v["format_version"] = 4.into();
    Ok(v)
}

/// v3 (RFC 0007): the controller's `target` and `knob_mode` became each
/// seat's focus and knob page. A v2 project has no seats, so both are
/// dropped: seats start focused on the first instrument, on its first page.
fn v2_to_v3(mut v: Value) -> Result<Value, String> {
    if let Some(c) = v.get_mut("controller").and_then(Value::as_object_mut) {
        c.remove("target");
        c.remove("knob_mode");
    }
    v["seats"] = Value::Object(Default::default());
    v["format_version"] = 3.into();
    Ok(v)
}

const _: () = assert!(MIGRATIONS.len() as u32 + OLDEST_PROJECT_FORMAT_VERSION == PROJECT_FORMAT_VERSION);

/// Upgrade raw project JSON to the current format version.
pub fn migrate_project(mut v: Value) -> Result<Value, String> {
    let version = v
        .get("format_version")
        .and_then(Value::as_u64)
        .ok_or("project is missing format_version")? as u32;
    if version < OLDEST_PROJECT_FORMAT_VERSION {
        return Err(format!(
            "unsupported project format_version {version}: projects from before RFC 0004 (instruments and \
             the channel mixer) are no longer supported; create a new project"
        ));
    }
    if version > PROJECT_FORMAT_VERSION {
        return Err(format!(
            "unsupported project format_version {version} (this build supports up to {PROJECT_FORMAT_VERSION})"
        ));
    }
    for m in &MIGRATIONS[(version - OLDEST_PROJECT_FORMAT_VERSION) as usize..] {
        v = m(v)?;
    }
    Ok(v)
}

pub fn parse_project(json: &str) -> Result<ProjectFile, String> {
    let raw: Value = serde_json::from_str(json).map_err(|e| format!("invalid JSON: {e}"))?;
    let v = migrate_project(raw)?;
    serde_json::from_value(v).map_err(|e| format!("invalid project: {e}"))
}

/// Pretty JSON with stable key order and a trailing newline.
pub fn project_to_json(p: &ProjectFile) -> String {
    let mut s = serde_json::to_string_pretty(p).expect("project serializes");
    s.push('\n');
    s
}

/// Format a track's steps for a project file: the active length, extended to
/// the next multiple of 16 if notes exist beyond it (so they are not lost).
pub fn steps_for_file(steps: &[u8], length: usize) -> String {
    let last = steps.iter().rposition(|s| *s != STEP_OFF).map(|i| i + 1).unwrap_or(0);
    let mut end = length.max(1);
    if last > end {
        end = last.div_ceil(16) * 16;
    }
    format_steps(&steps[..end.min(steps.len())])
}

#[cfg(test)]
mod tests {
    use super::*;

    const V2_FIXTURE: &str = include_str!("../fixtures/project-v2.json");

    #[test]
    fn v2_fixture_loads() {
        let p = parse_project(V2_FIXTURE).unwrap();
        assert_eq!(p.format_version, 5);
        assert!(p.seats.is_empty());
        assert!(p.controller.follow);
        let drums = &p.tracks["drums"];
        assert_eq!(drums.selected, 1);
        let Some(ProjectPattern::Drums(d)) = &drums.clips[&1].pattern else { panic!("drums pattern") };
        assert_eq!(d[&Voice::Kick], "X---x---X---x---");
        assert!(matches!(&p.tracks["bass"].clips[&1].pattern, Some(ProjectPattern::Notes(n)) if n.starts_with("C2")));
        assert_eq!(p.routes["bass"], 2);
        assert_eq!(p.params["transport.tempo"], 118.0);
    }

    #[test]
    fn round_trip_is_stable() {
        let p = parse_project(V2_FIXTURE).unwrap();
        let s = project_to_json(&p);
        assert_eq!(parse_project(&s).unwrap(), p);
        assert_eq!(project_to_json(&parse_project(&s).unwrap()), s);
    }

    #[test]
    fn rejects_future_versions() {
        let e = parse_project(r#"{"format_version": 999}"#).unwrap_err();
        assert!(e.contains("unsupported"));
    }

    #[test]
    fn steps_trimmed_but_not_lost() {
        let mut steps = vec![0u8; MAX_STEPS];
        steps[0] = 1;
        assert_eq!(steps_for_file(&steps, 16).len(), 16);
        steps[20] = 2;
        assert_eq!(steps_for_file(&steps, 16).len(), 32);
    }
}
