//! Known device models (RFC 0007): data files in `crates/daemon/devices/`,
//! embedded at build time. A model says which ports of a controller to use,
//! what to call them, and a default layout that seats use until they bind
//! the device themselves. Adding a controller is usually just a file here.

use fours_protocol::DeviceModel;
use std::sync::OnceLock;

const FILES: &[(&str, &str)] = &[
    ("akai-mpk-mini-iv.json", include_str!("../devices/akai-mpk-mini-iv.json")),
    ("livid-block.json", include_str!("../devices/livid-block.json")),
];

/// Every known model, parsed once.
pub fn all() -> &'static [DeviceModel] {
    static MODELS: OnceLock<Vec<DeviceModel>> = OnceLock::new();
    MODELS.get_or_init(|| {
        FILES
            .iter()
            .map(|(file, json)| serde_json::from_str(json).unwrap_or_else(|e| panic!("devices/{file}: {e}")))
            .collect()
    })
}

pub fn get(id: &str) -> Option<&'static DeviceModel> {
    all().iter().find(|m| m.id == id)
}

/// The model a port belongs to, and the name it gives that port (`None`
/// for a port the model ignores).
pub fn for_port(port: &str) -> Option<(&'static DeviceModel, Option<&'static str>)> {
    let p = port.to_lowercase();
    let m = all().iter().find(|m| p.contains(&m.matches.to_lowercase()))?;
    Some((m, m.role(port)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every shipped model parses and its layout passes the seat checks.
    #[test]
    fn models_parse_and_validate() {
        for m in all() {
            crate::core::check_layout(&m.layout).unwrap_or_else(|e| panic!("{}: {}", m.id, e.message));
        }
        let (m, role) = for_port("MPK mini IV DAW Port").unwrap();
        assert_eq!((m.id.as_str(), role), ("akai_mpk_mini_iv", Some("mpk_daw")));
        assert_eq!(for_port("MPK mini IV Din Port").unwrap().1, None);
        assert_eq!(for_port("block Controls").unwrap().1, Some("block"));
    }
}
