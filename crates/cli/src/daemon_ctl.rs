//! Local daemon lifecycle: start `4sd` in the background, stop it, report
//! status, and show logs. Uses the runtime file `<data-dir>/4sd.json` that the
//! daemon writes while running. See docs/lifecycle.md.

use anyhow::{Context, Result, anyhow, bail};
use clap::Args;
use fours_protocol::{DaemonInfo, RUNTIME_FILE};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Args, Debug, Clone, Default)]
pub struct StartArgs {
    /// Address to listen on (127.0.0.1:0 picks a free port).
    #[arg(long, default_value = fours_protocol::DEFAULT_LISTEN)]
    pub listen: String,
    /// Run without an audio device.
    #[arg(long)]
    pub no_audio: bool,
    /// Disable MIDI device auto-connect.
    #[arg(long)]
    pub no_midi: bool,
    /// Project bundle to load at startup.
    #[arg(long)]
    pub project: Option<String>,
    /// Path to the 4sd binary (default: FOURSD_BIN, next to `4s`, or PATH).
    #[arg(long)]
    pub bin: Option<PathBuf>,
}

pub fn default_data_dir() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into())).join(".4s")
}

pub fn log_path(data_dir: &Path) -> PathBuf {
    data_dir.join("logs").join("4sd.log")
}

pub fn read_runtime(data_dir: &Path) -> Option<DaemonInfo> {
    let s = std::fs::read_to_string(data_dir.join(RUNTIME_FILE)).ok()?;
    serde_json::from_str(&s).ok()
}

pub fn pid_alive(pid: u32) -> bool {
    let r = unsafe { libc::kill(pid as libc::pid_t, 0) };
    r == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// The running daemon for this data dir, if any. Removes a stale runtime file.
pub fn live(data_dir: &Path) -> Option<DaemonInfo> {
    let info = read_runtime(data_dir)?;
    if pid_alive(info.pid) {
        Some(info)
    } else {
        let _ = std::fs::remove_file(data_dir.join(RUNTIME_FILE));
        None
    }
}

fn find_bin(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(p) = explicit {
        return Ok(p.to_path_buf());
    }
    if let Ok(p) = std::env::var("FOURSD_BIN") {
        return Ok(PathBuf::from(p));
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let sibling = dir.join("4sd");
        if sibling.exists() {
            return Ok(sibling);
        }
    }
    Ok(PathBuf::from("4sd")) // rely on PATH
}

pub fn tail(path: &Path, n: usize) -> String {
    let s = std::fs::read_to_string(path).unwrap_or_default();
    let lines: Vec<&str> = s.lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

/// Start 4sd in the background. Returns the running daemon's info and whether
/// this call started it.
pub fn start(data_dir: &Path, args: &StartArgs, token: Option<&str>) -> Result<(DaemonInfo, bool)> {
    if let Some(info) = live(data_dir) {
        return Ok((info, false));
    }
    std::fs::create_dir_all(data_dir)?;
    let log = log_path(data_dir);
    std::fs::create_dir_all(log.parent().unwrap())?;
    let out = std::fs::OpenOptions::new().create(true).append(true).open(&log)?;

    let bin = find_bin(args.bin.as_deref())?;
    let mut cmd = Command::new(&bin);
    cmd.arg("--listen")
        .arg(&args.listen)
        .arg("--data-dir")
        .arg(data_dir)
        .arg("--log-file")
        .arg(&log);
    if args.no_audio {
        cmd.arg("--no-audio");
    }
    if args.no_midi {
        cmd.arg("--no-midi");
    }
    if let Some(p) = &args.project {
        // Resolve relative to our working directory, not the daemon's.
        let p = std::fs::canonicalize(p).unwrap_or_else(|_| PathBuf::from(p));
        cmd.arg("--project").arg(p);
    }
    if let Some(t) = token {
        cmd.env("FOURS_TOKEN", t);
    }
    cmd.stdin(Stdio::null()).stdout(Stdio::from(out.try_clone()?)).stderr(Stdio::from(out));
    // Own process group: Ctrl-C or closing the terminal does not reach it.
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut cmd, 0);

    let mut child = cmd.spawn().with_context(|| format!("failed to run {}", bin.display()))?;
    let pid = child.id();
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(status) = child.try_wait()? {
            bail!("4sd exited during startup ({status}). Log tail:\n{}", tail(&log, 15));
        }
        if let Some(info) = read_runtime(data_dir)
            && info.pid == pid
        {
            return Ok((info, true));
        }
        if Instant::now() > deadline {
            bail!("4sd (pid {pid}) did not become ready within 15s. Log tail:\n{}", tail(&log, 15));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Wait for `pid` to exit.
pub fn wait_exit(pid: u32, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if !pid_alive(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    !pid_alive(pid)
}

/// Signal-based stop, used when the RPC shutdown did not work.
pub fn signal_stop(pid: u32, force: bool) -> Result<()> {
    unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
    if wait_exit(pid, Duration::from_secs(3)) {
        return Ok(());
    }
    if force {
        unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
        if wait_exit(pid, Duration::from_secs(2)) {
            return Ok(());
        }
    }
    Err(anyhow!("4sd (pid {pid}) is still running; retry with --force"))
}

pub fn format_uptime(secs: f64) -> String {
    let s = secs as u64;
    match s {
        0..=59 => format!("{s}s"),
        60..=3599 => format!("{}m{:02}s", s / 60, s % 60),
        _ => format!("{}h{:02}m", s / 3600, (s % 3600) / 60),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_runtime_file_is_ignored_and_removed() {
        let dir = std::env::temp_dir().join(format!("4s-cli-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut info = DaemonInfo {
            pid: std::process::id(),
            version: "0".into(),
            protocol_version: 1,
            url: "ws://127.0.0.1:1".into(),
            role: fours_protocol::Role::Engine,
            data_dir: dir.to_string_lossy().into(),
            log_file: None,
            started_at: 0.0,
            uptime: 0.0,
        };
        let write = |i: &DaemonInfo| std::fs::write(dir.join(RUNTIME_FILE), serde_json::to_string(i).unwrap()).unwrap();
        write(&info);
        assert_eq!(live(&dir).map(|i| i.pid), Some(std::process::id()), "our own pid is alive");
        info.pid = 999_999; // almost certainly not running
        write(&info);
        assert!(live(&dir).is_none());
        assert!(!dir.join(RUNTIME_FILE).exists(), "stale file removed");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn uptime_format() {
        assert_eq!(format_uptime(5.0), "5s");
        assert_eq!(format_uptime(125.0), "2m05s");
        assert_eq!(format_uptime(7300.0), "2h01m");
    }
}
