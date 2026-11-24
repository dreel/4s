// @ts-check
// The assistant's session log, for debugging: one JSONL file per chat in
// `<data dir>/assistant/sessions/`, with every message, model reply, tool
// call (input, result or error, refusals too) and turn end. The daemon's
// journal only has the edits that reached it; this has the rest. It holds
// project data (what the tools returned) and stays on this machine; the
// API key is never written. The newest KEEP files are kept.

const fs = require("node:fs");
const path = require("node:path");

/** Session files kept; older ones are deleted when a new one starts. */
const KEEP = 50;

class SessionLog {
  /** @param {string} dir */
  constructor(dir) {
    this.dir = dir;
    /** @type {string | null} the current file, made on the first write */
    this.file = null;
  }

  /** Start a new file on the next write (the panel's "New chat"). */
  rotate() {
    this.file = null;
  }

  /**
   * Append one record. Logging never breaks a turn: errors are swallowed.
   * @param {Record<string, unknown>} record
   */
  write(record) {
    try {
      if (!this.file) this.open();
      fs.appendFileSync(/** @type {string} */ (this.file), `${JSON.stringify({ time: new Date().toISOString(), ...record })}\n`, { mode: 0o600 });
    } catch {
      // a full disk or unwritable data dir must not stop the assistant
    }
  }

  open() {
    fs.mkdirSync(this.dir, { recursive: true, mode: 0o700 });
    const stamp = new Date().toISOString().replace(/[:.]/g, "-");
    this.file = path.join(this.dir, `${stamp}-${process.pid}.jsonl`);
    const old = fs
      .readdirSync(this.dir)
      .filter((f) => f.endsWith(".jsonl"))
      .sort()
      .reverse()
      .slice(KEEP - 1);
    for (const f of old) fs.rmSync(path.join(this.dir, f), { force: true });
  }
}

/** A log that writes nothing (tests that do not look at it). */
const NO_LOG = { write() {}, rotate() {} };

module.exports = { SessionLog, NO_LOG, KEEP };
