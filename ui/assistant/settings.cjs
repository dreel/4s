// @ts-check
// The assistant's settings: the API key and the model. The key is
// encrypted with Electron safeStorage (the macOS keychain, DPAPI, or the
// Linux secret store) and never sent to the renderer. Where encryption is
// not available, the key is not written anywhere: it is kept for this
// session only. Without a stored key, ANTHROPIC_API_KEY is used if set.

const fs = require("node:fs");
const path = require("node:path");
const { MODELS, DEFAULT_MODEL } = require("./loop.cjs");

class Settings {
  /**
   * @param {string} dir where settings.json lives (`<data dir>/assistant`)
   * @param {{ isEncryptionAvailable(): boolean, encryptString(s: string): Buffer, decryptString(b: Buffer): string }} safeStorage
   */
  constructor(dir, safeStorage) {
    this.file = path.join(dir, "settings.json");
    this.safeStorage = safeStorage;
    /** @type {string | null} a key kept for this session only */
    this.sessionKey = null;
    /** @type {{ model?: string, key?: string }} */
    this.data = {};
    try {
      this.data = JSON.parse(fs.readFileSync(this.file, "utf8"));
    } catch {
      // first run, or unreadable: defaults
    }
  }

  save() {
    fs.mkdirSync(path.dirname(this.file), { recursive: true });
    fs.writeFileSync(this.file, JSON.stringify(this.data, null, 2), { mode: 0o600 });
  }

  model() {
    return this.data.model && MODELS.includes(this.data.model) ? this.data.model : DEFAULT_MODEL;
  }

  /** @param {string} model */
  setModel(model) {
    if (!MODELS.includes(model)) throw new Error(`unknown model ${model}`);
    this.data.model = model;
    this.save();
  }

  /** The key to use, or null. Never leaves the main process. */
  apiKey() {
    if (this.sessionKey) return this.sessionKey;
    if (this.data.key && this.safeStorage.isEncryptionAvailable()) {
      try {
        return this.safeStorage.decryptString(Buffer.from(this.data.key, "base64"));
      } catch {
        return null;
      }
    }
    return process.env.ANTHROPIC_API_KEY || null;
  }

  /** Where the key comes from, for the settings popover. */
  keySource() {
    if (this.sessionKey) return "session";
    if (this.data.key) return "keychain";
    if (process.env.ANTHROPIC_API_KEY) return "env";
    return "none";
  }

  /**
   * Store a key (encrypted), or forget it with null. Without encryption
   * the key is kept for this session only.
   * @param {string | null} key
   */
  setKey(key) {
    const k = key?.trim() || null;
    this.sessionKey = null;
    delete this.data.key;
    if (k) {
      if (this.safeStorage.isEncryptionAvailable()) this.data.key = this.safeStorage.encryptString(k).toString("base64");
      else this.sessionKey = k;
    }
    this.save();
  }

  /** What the renderer may see. */
  view() {
    return {
      model: this.model(),
      models: MODELS,
      keySource: this.keySource(),
      encryption: this.safeStorage.isEncryptionAvailable(),
    };
  }
}

module.exports = { Settings };
