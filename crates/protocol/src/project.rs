//! Project file format (`<name>.4s/project.json`). See docs/project-format.md.

use crate::types::*;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use ts_rs::TS;

/// Current project format version. Bump and add a migration when the format
/// changes incompatibly.
pub const PROJECT_FORMAT_VERSION: u32 = 1;
pub const PROJECT_FILE_NAME: &str = "project.json";
pub const PROJECT_EXTENSION: &str = "4s";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
pub struct ProjectFile {
    pub format_version: u32,
    /// Parameter values by path. Unknown paths are ignored on load; missing
    /// paths take their defaults.
    pub params: BTreeMap<String, f64>,
    /// Step strings per voice, e.g. `"kick": "x---x---x---x---"`.
    pub patterns: BTreeMap<Voice, String>,
    pub controller: ProjectController,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
pub struct ProjectController {
    pub knob_mode: KnobMode,
    pub follow: bool,
}

/// A migration upgrades a project from version N to N+1. `MIGRATIONS[0]`
/// upgrades v1 -> v2, and so on.
type Migration = fn(Value) -> Result<Value, String>;
const MIGRATIONS: &[Migration] = &[];

const _: () = assert!(MIGRATIONS.len() as u32 + 1 == PROJECT_FORMAT_VERSION);

/// Upgrade raw project JSON to the current format version.
pub fn migrate_project(mut v: Value) -> Result<Value, String> {
    let version = v
        .get("format_version")
        .and_then(Value::as_u64)
        .ok_or("project is missing format_version")? as u32;
    if version == 0 || version > PROJECT_FORMAT_VERSION {
        return Err(format!(
            "unsupported project format_version {version} (this build supports up to {PROJECT_FORMAT_VERSION})"
        ));
    }
    for m in &MIGRATIONS[(version as usize - 1)..] {
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

    const V1_FIXTURE: &str = include_str!("../fixtures/project-v1.json");

    #[test]
    fn v1_fixture_loads() {
        let p = parse_project(V1_FIXTURE).unwrap();
        assert_eq!(p.format_version, 1);
        assert_eq!(p.patterns[&Voice::Kick], "X---x---X---x---");
        assert_eq!(p.params["transport.tempo"], 118.0);
    }

    #[test]
    fn round_trip_is_stable() {
        let p = parse_project(V1_FIXTURE).unwrap();
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
