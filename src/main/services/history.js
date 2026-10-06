// The local history: one JSON line per event in history.jsonl, photos beside
// it in photos/, both under the folder the History tab points at. Entries
// older than the retention period are dropped once an hour, photos and all.
//
// Pure file handling, no database, so the folder can be opened in Explorer,
// copied to a USB stick, or pointed at a network share.
const fs = require('fs');
const path = require('path');
const { EventEmitter } = require('events');

const LOG_FILE = 'history.jsonl';
const PHOTO_DIR = 'photos';
const PRUNE_INTERVAL_MS = 60 * 60 * 1000;

const pad = (n) => String(n).padStart(2, '0');
const stamp = (d) => `${d.getFullYear()}${pad(d.getMonth() + 1)}${pad(d.getDate())}-${pad(d.getHours())}${pad(d.getMinutes())}${pad(d.getSeconds())}`;

class History extends EventEmitter {
  constructor({ folder, retentionDays = 7, savePhotos = true, saveLog = true }) {
    super();
    this.configure({ folder, retentionDays, savePhotos, saveLog });
    this.pruneTimer = null;
  }

  configure({ folder, retentionDays, savePhotos, saveLog }) {
    this.folder = folder;
    this.retentionDays = Math.max(1, Number(retentionDays) || 7);
    this.savePhotos = savePhotos !== false;
    this.saveLog = saveLog !== false;
    fs.mkdirSync(path.join(this.folder, PHOTO_DIR), { recursive: true });
  }

  get logPath() { return path.join(this.folder, LOG_FILE); }
  photoPath(name) { return path.join(this.folder, PHOTO_DIR, name); }

  start() {
    this.prune();
    this.pruneTimer = setInterval(() => this.prune(), PRUNE_INTERVAL_MS);
    this.pruneTimer.unref?.();
    return this;
  }

  stop() { clearInterval(this.pruneTimer); }

  // Write an event. photo is a JPEG buffer; it is saved beside the log and
  // the entry carries its file name. Returns the entry as written.
  record(event, photo = null) {
    const at = event.at ? new Date(event.at) : new Date();
    const entry = {
      id: `${stamp(at)}-${Math.random().toString(36).slice(2, 7)}`,
      at: at.toISOString(),
      type: event.type || 'weighing',
      weight: event.weight ?? null,
      unit: event.unit || 'lb',
      plate: event.plate || null,
      vehicle: event.vehicle || null,
      photo: null,
      note: event.note || ''
    };
    if (photo && this.savePhotos) {
      entry.photo = `${entry.id}.jpg`;
      fs.writeFileSync(this.photoPath(entry.photo), photo);
    }
    if (this.saveLog) fs.appendFileSync(this.logPath, `${JSON.stringify(entry)}\n`);
    this.emit('entry', entry);
    return entry;
  }

  // Change fields on an entry already written (a better plate read). The
  // log is rewritten; it is small, one line per truck.
  update(id, patch) {
    const entries = this.read();
    const index = entries.findIndex((e) => e.id === id);
    if (index === -1) return null;
    entries[index] = { ...entries[index], ...patch };
    this.writeAll(entries);
    this.emit('entry', entries[index]);
    return entries[index];
  }

  read() {
    try {
      return fs.readFileSync(this.logPath, 'utf8').split('\n').filter(Boolean).map((line) => {
        try { return JSON.parse(line); } catch { return null; }
      }).filter(Boolean);
    } catch { return []; }
  }

  // Newest first, limited.
  list({ limit = 200 } = {}) {
    return this.read().reverse().slice(0, limit);
  }

  writeAll(entries) {
    const tmp = `${this.logPath}.tmp`;
    fs.writeFileSync(tmp, entries.map((e) => JSON.stringify(e)).join('\n') + (entries.length ? '\n' : ''));
    fs.renameSync(tmp, this.logPath);
  }

  // Drop what is older than the retention period: entries from the log and
  // the photos they named. Photos nobody names any more go too.
  prune(now = Date.now()) {
    const cutoff = now - this.retentionDays * 24 * 60 * 60 * 1000;
    const entries = this.read();
    const keep = entries.filter((e) => new Date(e.at).getTime() >= cutoff);
    const dropped = entries.length - keep.length;
    if (dropped > 0) this.writeAll(keep);
    const named = new Set(keep.map((e) => e.photo).filter(Boolean));
    let photosRemoved = 0;
    try {
      for (const name of fs.readdirSync(path.join(this.folder, PHOTO_DIR))) {
        if (!/\.jpg$/i.test(name) || named.has(name)) continue;
        const full = this.photoPath(name);
        try {
          if (fs.statSync(full).mtimeMs < cutoff || !named.has(name)) { fs.unlinkSync(full); photosRemoved += 1; }
        } catch {}
      }
    } catch {}
    if (dropped || photosRemoved) this.emit('pruned', { dropped, photosRemoved });
    return { dropped, photosRemoved };
  }

  // Sizes for the History tab.
  stats() {
    const entries = this.read();
    let bytes = 0;
    try {
      for (const name of fs.readdirSync(path.join(this.folder, PHOTO_DIR))) {
        try { bytes += fs.statSync(this.photoPath(name)).size; } catch {}
      }
    } catch {}
    return { entries: entries.length, photoBytes: bytes, oldest: entries[0]?.at || null, newest: entries[entries.length - 1]?.at || null, folder: this.folder };
  }
}

module.exports = { History, LOG_FILE, PHOTO_DIR };
