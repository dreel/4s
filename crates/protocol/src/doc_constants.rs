//! Protocol constants named in doc comments (`PPQ`, `MAX_CLIP_TICKS`, ...)
//! get their values written next to them in the generated JSON Schema, so a
//! reader of the schema (the CLI's `describe`, the in-app assistant, an
//! agent) sees `PPQ` (96) instead of a name it cannot look up. The values
//! come from the constants themselves, so the docs cannot drift from them.

use crate::*;

/// The constants a schema description may name in backticks, with the text
/// written after the name.
pub fn doc_constants() -> Vec<(&'static str, String)> {
    let gm = Voice::ALL.iter().map(|v| format!("{} {}", v.id(), v.gm_note())).collect::<Vec<_>>().join(", ");
    vec![
        ("PPQ", PPQ.to_string()),
        ("TICKS_PER_STEP", TICKS_PER_STEP.to_string()),
        ("TICKS_PER_BAR", TICKS_PER_BAR.to_string()),
        ("MAX_STEPS", MAX_STEPS.to_string()),
        ("MAX_CLIP_TICKS", MAX_CLIP_TICKS.to_string()),
        ("DEFAULT_CLIP_LENGTH", DEFAULT_CLIP_LENGTH.to_string()),
        ("MAX_EVENTS", MAX_EVENTS.to_string()),
        ("MAX_CLIPS", MAX_CLIPS.to_string()),
        ("MAX_SONG_TICKS", MAX_SONG_TICKS.to_string()),
        ("MAX_LANES", MAX_LANES.to_string()),
        ("MAX_LANE_POINTS", MAX_LANE_POINTS.to_string()),
        ("MAX_UPLOAD_BYTES", MAX_UPLOAD_BYTES.to_string()),
        ("MAX_INSTRUMENTS", MAX_INSTRUMENTS.to_string()),
        ("MAX_CHANNELS", MAX_CHANNELS.to_string()),
        ("SAMPLER_PADS", SAMPLER_PADS.to_string()),
        ("VEL_ON", VEL_ON.to_string()),
        ("VEL_ACCENT", VEL_ACCENT.to_string()),
        ("VEL_ACCENT_MIN", VEL_ACCENT_MIN.to_string()),
        ("GM_DRUM_NOTES", gm),
    ]
}

/// Write each named constant's value after its name in every `description`
/// in `schema`: `` `PPQ` `` becomes `` `PPQ` (96) ``.
pub fn resolve_doc_constants(schema: &mut serde_json::Value) {
    let table = doc_constants();
    walk(schema, &table);
}

fn walk(v: &mut serde_json::Value, table: &[(&'static str, String)]) {
    match v {
        serde_json::Value::Object(m) => {
            for (k, child) in m.iter_mut() {
                match child {
                    serde_json::Value::String(s) if k == "description" => {
                        // Matched with its backticks, so `TICKS_PER_STEP`
                        // never matches inside another name.
                        for (name, value) in table {
                            *s = s.replace(&format!("`{name}`"), &format!("`{name}` ({value})"));
                        }
                    }
                    _ => walk(child, table),
                }
            }
        }
        serde_json::Value::Array(a) => a.iter_mut().for_each(|c| walk(c, table)),
        _ => {}
    }
}

/// Backticked ALL_CAPS names in descriptions that `doc_constants` has no
/// value for. gen-bindings refuses to write a schema with any.
pub fn unresolved_doc_constants(schema: &serde_json::Value) -> Vec<String> {
    let known: Vec<&str> = doc_constants().into_iter().map(|(n, _)| n).collect();
    let mut out = Vec::new();
    collect(schema, &known, &mut out);
    out.sort();
    out.dedup();
    out
}

fn collect(v: &serde_json::Value, known: &[&str], out: &mut Vec<String>) {
    match v {
        serde_json::Value::Object(m) => {
            for (k, child) in m {
                match child {
                    serde_json::Value::String(s) if k == "description" => {
                        for (i, part) in s.split('`').enumerate() {
                            if i % 2 == 1
                                && part.len() > 2
                                && part.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
                                && !known.contains(&part)
                            {
                                out.push(part.to_string());
                            }
                        }
                    }
                    _ => collect(child, known, out),
                }
            }
        }
        serde_json::Value::Array(a) => a.iter().for_each(|c| collect(c, known, out)),
        _ => {}
    }
}
