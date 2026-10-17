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
pub const PROJECT_FORMAT_VERSION: u32 = 8;
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
    /// Audio effects (RFC 0010), each chain in order (channel 0 is the
    /// master). Their parameters are in `params` under their ids.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub devices: Vec<ProjectDevice>,
    /// Modulation routes (RFC 0010).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mod_routes: Vec<crate::device::ModRoute>,
    /// Samplers' samples by instrument id and pad: a path inside the
    /// bundle (`samples/...`) once saved, else where it is on the engine
    /// host; with a region, or where a bundle's copy came from (RFC 0011).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub samples: BTreeMap<String, BTreeMap<u32, ProjectSample>>,
    /// Parameter values by path. Unknown paths are ignored on load; missing
    /// paths take their defaults.
    pub params: BTreeMap<String, f64>,
    /// Per instrument id (RFC 0008): its clip pool, selected clip,
    /// arrangement, and launched clip. An instrument left out has one empty
    /// one-bar clip and follows its (empty) arrangement.
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

/// An audio effect in a project file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
pub struct ProjectDevice {
    pub id: String,
    #[serde(rename = "type")]
    #[ts(rename = "type")]
    pub kind: crate::device::DeviceType,
    pub name: String,
    /// Whose insert chain: a channel number, or 0 for the master; absent
    /// for a modulator.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<u32>,
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
    /// The clip the track loops instead of its arrangement (RFC 0009).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launched: Option<u32>,
}

/// A clip in a project file: its length, and a step pattern when one says
/// exactly the same thing (drum step strings per voice, `{"kick":
/// "x---x---"}`, or a note string, `"C2 C2! D#2~ -"`), else events in text
/// form (`tick:note:len:vel`, see `format_events`); both absent for an
/// empty clip.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
pub struct ProjectClip {
    /// Default: the clip's id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Ticks (where it loops).
    #[serde(default = "default_clip_length")]
    pub length: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern: Option<ProjectPattern>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub events: Option<String>,
    /// Automation lanes (RFC 0010).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lanes: Vec<crate::clip::Lane>,
}

impl Default for ProjectClip {
    /// An empty one-bar clip.
    fn default() -> Self {
        Self { name: None, length: default_clip_length(), pattern: None, events: None, lanes: Vec::new() }
    }
}

fn default_clip_length() -> u32 {
    crate::clip::DEFAULT_CLIP_LENGTH
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
pub struct ProjectController {
    /// The grid page follows the playhead.
    pub follow: bool,
}

/// A pad's sample in a project: the file it plays (a path, as in v7, for
/// all of it), or a region of it and where a bundle's copy came from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
#[serde(untagged)]
pub enum ProjectSample {
    Path(String),
    Region(ProjectSampleRegion),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
pub struct ProjectSampleRegion {
    pub path: String,
    /// Seconds of the file (default: all of it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<f64>,
    /// For the bundle's copy of a region: the file and region it was cut
    /// from (on the engine host that saved it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<crate::device::SampleOrigin>,
}

impl ProjectSample {
    pub fn path(&self) -> &str {
        match self {
            Self::Path(p) => p,
            Self::Region(r) => &r.path,
        }
    }

    pub fn path_mut(&mut self) -> &mut String {
        match self {
            Self::Path(p) => p,
            Self::Region(r) => &mut r.path,
        }
    }
}

/// A migration upgrades a project from version N to N+1.
/// `MIGRATIONS[0]` upgrades `OLDEST_PROJECT_FORMAT_VERSION` to the next one,
/// and so on.
type Migration = fn(Value) -> Result<Value, String>;
const MIGRATIONS: &[Migration] = &[v2_to_v3, v3_to_v4, v4_to_v5, v5_to_v6, v6_to_v7, v7_to_v8];

/// v8 (RFC 0011): a pad's sample can be a region of a file, and a
/// bundle's copy can say where it came from. A v7 sample (a path) is still
/// read as all of that file.
fn v7_to_v8(mut v: Value) -> Result<Value, String> {
    v["format_version"] = 8.into();
    Ok(v)
}

/// v7 (RFC 0010): audio effects (`devices`). A v6 project has none.
fn v6_to_v7(mut v: Value) -> Result<Value, String> {
    v["format_version"] = 7.into();
    Ok(v)
}

/// v6 (RFC 0009): one transport that always plays the arrangement, and clip
/// launching instead of pattern mode. `song.mode` and `sequencer.length`
/// go. A project saved in pattern mode launches each track's selected clip
/// (an instrument with no track gets its empty clip, launched), so it plays
/// as it did. A clip without a length gets `sequencer.length` (in ticks),
/// which it followed.
fn v5_to_v6(mut v: Value) -> Result<Value, String> {
    let params = v.get_mut("params").and_then(Value::as_object_mut);
    let (song_mode, steps) = match params {
        Some(p) => (
            p.remove("song.mode").and_then(|m| m.as_f64()).unwrap_or(0.0),
            p.remove("sequencer.length").and_then(|l| l.as_f64()).unwrap_or(16.0),
        ),
        None => (0.0, 16.0),
    };
    let length = ((steps.round() as i64).clamp(1, MAX_STEPS as i64) as u32) * crate::clip::TICKS_PER_STEP;
    let pattern = song_mode < 0.5;
    let ids: Vec<String> = v
        .get("instruments")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|i| i.get("id").and_then(Value::as_str).map(str::to_string)).collect())
        .unwrap_or_default();
    if !v.get("tracks").is_some_and(Value::is_object) {
        v["tracks"] = Value::Object(Default::default());
    }
    let tracks = v["tracks"].as_object_mut().expect("an object");
    if pattern {
        for id in &ids {
            tracks.entry(id.clone()).or_insert_with(|| serde_json::json!({ "selected": 1, "clips": { "1": {} } }));
        }
    }
    for t in tracks.values_mut() {
        if let Some(clips) = t.get_mut("clips").and_then(Value::as_object_mut) {
            for c in clips.values_mut() {
                if let Some(c) = c.as_object_mut()
                    && c.get("length").is_none_or(Value::is_null)
                {
                    c.insert("length".into(), length.into());
                }
            }
        }
        if pattern && let Some(selected) = t.get("selected").cloned() {
            t["launched"] = selected;
        }
    }
    v["format_version"] = 6.into();
    Ok(v)
}

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

/// Format a track's steps for a project file: the clip's length in steps,
/// extended to the next multiple of 16 if notes exist beyond it (so they
/// are not lost).
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
        assert_eq!(p.format_version, 8);
        assert!(p.seats.is_empty());
        assert!(p.controller.follow);
        let drums = &p.tracks["drums"];
        assert_eq!(drums.selected, 1);
        // Saved in pattern mode: it launches its clip, which loops at the
        // project's `sequencer.length`.
        assert_eq!(drums.launched, Some(1));
        assert_eq!(drums.clips[&1].length, 16 * crate::clip::TICKS_PER_STEP);
        assert!(!p.params.contains_key("sequencer.length"));
        let Some(ProjectPattern::Drums(d)) = &drums.clips[&1].pattern else { panic!("drums pattern") };
        assert_eq!(d[&Voice::Kick], "X---x---X---x---");
        assert!(matches!(&p.tracks["bass"].clips[&1].pattern, Some(ProjectPattern::Notes(n)) if n.starts_with("C2")));
        assert_eq!(p.routes["bass"], 2);
        assert_eq!(p.params["transport.tempo"], 118.0);
    }

    /// v6 (RFC 0009): a project saved in song mode launches nothing; one
    /// saved in pattern mode launches every track's selected clip, an
    /// instrument without a track included. Clips that followed
    /// `sequencer.length` get it as their length.
    #[test]
    fn v5_pattern_mode_launches_selected_clips() {
        let v5 = |mode: f64| {
            format!(
                r#"{{"format_version": 5,
                "instruments": [{{"id": "drums", "type": "tr808", "name": "Drums"}}, {{"id": "bass", "type": "tb303", "name": "Bass"}}],
                "channels": [], "routes": {{}}, "controller": {{"follow": true}},
                "params": {{"song.mode": {mode}, "sequencer.length": 12, "transport.tempo": 100}},
                "tracks": {{"drums": {{"selected": 2, "clips": {{"1": {{}}, "2": {{"length": 48}}}},
                    "arrangement": [{{"clip": 1, "start": 0, "length": 384}}]}}}}}}"#
            )
        };
        let pattern = parse_project(&v5(0.0)).unwrap();
        assert_eq!(pattern.tracks["drums"].launched, Some(2));
        assert_eq!(pattern.tracks["bass"].launched, Some(1));
        assert_eq!(pattern.tracks["drums"].clips[&1].length, 12 * crate::clip::TICKS_PER_STEP);
        assert_eq!(pattern.tracks["drums"].clips[&2].length, 48);
        assert_eq!(pattern.params.keys().collect::<Vec<_>>(), ["transport.tempo"]);
        let song = parse_project(&v5(1.0)).unwrap();
        assert_eq!(song.tracks["drums"].launched, None);
        assert!(!song.tracks.contains_key("bass"));
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
