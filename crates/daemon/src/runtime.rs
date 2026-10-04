//! The runtime file (`<data-dir>/4sd.json`): written while the daemon runs so
//! local tools can find it, and used to enforce one daemon per data dir.

use fours_protocol::{DaemonInfo, RUNTIME_FILE};
use std::path::{Path, PathBuf};

pub fn path(data_dir: &Path) -> PathBuf {
    data_dir.join(RUNTIME_FILE)
}

pub fn read(data_dir: &Path) -> Option<DaemonInfo> {
    let s = std::fs::read_to_string(path(data_dir)).ok()?;
    serde_json::from_str(&s).ok()
}

pub fn write(data_dir: &Path, info: &DaemonInfo) -> std::io::Result<()> {
    let tmp = data_dir.join(format!("{RUNTIME_FILE}.tmp"));
    std::fs::write(&tmp, serde_json::to_string_pretty(info).unwrap() + "\n")?;
    std::fs::rename(tmp, path(data_dir))
}

/// Remove the runtime file if it still belongs to `pid`.
pub fn remove(data_dir: &Path, pid: u32) {
    if read(data_dir).is_some_and(|i| i.pid == pid) {
        let _ = std::fs::remove_file(path(data_dir));
    }
}

pub fn pid_alive(pid: u32) -> bool {
    // Signal 0 checks existence without sending anything. EPERM means the
    // process exists but belongs to someone else.
    let r = unsafe { libc::kill(pid as libc::pid_t, 0) };
    r == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}
