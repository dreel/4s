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

/// Backticked ALL_CAPS names left in descriptions that have no value in
/// `doc_constants` (after `resolve_doc_constants`).
pub fn unresolved_doc_constants(schema: &serde_json::Value) -> Vec<String> {
    let mut out = Vec::new();
    collect(schema, &mut out);
    out.sort();
    out.dedup();
    out
}

fn collect(v: &serde_json::Value, out: &mut Vec<String>) {
    match v {
        serde_json::Value::Object(m) => {
            for (k, child) in m {
                match child {
                    serde_json::Value::String(s) if k == "description" => {
                        for (i, part) in s.split('`').enumerate() {
                            let resolved = s.split('`').nth(i + 1).is_some_and(|after| after.starts_with(" ("));
                            if i % 2 == 1
                                && part.len() > 2
                                && part.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
                                && !resolved
                            {
                                out.push(part.to_string());
                            }
                        }
                    }
                    _ => collect(child, out),
                }
            }
        }
        serde_json::Value::Array(a) => a.iter().for_each(|c| collect(c, out)),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every constant a schema description names has its value written next
    /// to it, so `describe` never shows a bare name. Add new ones to
    /// `doc_constants`.
    #[test]
    fn schema_descriptions_name_only_known_constants() {
        let mut schema = serde_json::json!({
            "request": schemars::schema_for!(Request),
            "event": schemars::schema_for!(EventEnvelope),
            "snapshot": schemars::schema_for!(Snapshot),
            "project": schemars::schema_for!(ProjectFile),
        });
        resolve_doc_constants(&mut schema);
        assert_eq!(unresolved_doc_constants(&schema), Vec::<String>::new());
    }
}
