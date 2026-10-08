// A bug report from the yard PC: what the user typed, plus what the app
// knows - version, machine, settings with passwords removed, the last
// weighings and the tail of the log - filed as an issue on GitHub so it can
// be looked at from anywhere without a trip to the machine.
//
// Everything goes in the issue body (GitHub allows 65,536 characters), so a
// report is one small request: on the yard's connection that matters.
const os = require('os');
const { redactCredentials } = require('./redactCredentials');

const BODY_LIMIT = 60000;
const LOG_TAIL_CHARS = 40000;
const HISTORY_ENTRIES = 40;
const SECRET_KEYS = /password|token|secret|passwd/i;

// A copy of the settings with every password-like value blanked and any
// credentials inside URLs starred out.
const redactSettings = (value) => {
  if (Array.isArray(value)) return value.map(redactSettings);
  if (value && typeof value === 'object') {
    const out = {};
    for (const [k, v] of Object.entries(value)) out[k] = SECRET_KEYS.test(k) ? (v ? '***' : '') : redactSettings(v);
    return out;
  }
  return typeof value === 'string' ? redactCredentials(value) : value;
};

const fence = (text, lang = '') => `\`\`\`${lang}\n${String(text).replace(/```/g, "'''")}\n\`\`\``;

const titleFor = (description) => {
  const first = String(description || '').split(/\r?\n/).map((l) => l.trim()).find(Boolean) || 'Problem at the scale';
  return first.length > 80 ? `${first.slice(0, 77)}…` : first;
};

/**
 * @param {object} args
 * @param {string} args.description  what the user typed
 * @param {object} args.info         { version, electron, ffmpeg, plateReader, vehicleModel }
 * @param {object} args.settings     the station's settings
 * @param {string} args.log          the log text (its tail is kept)
 * @param {object[]} args.history    recent history entries, newest first
 * @param {object|null} args.scale   the scale reader's status
 */
const buildReport = ({ description, info = {}, settings = {}, log = '', history = [], scale = null, now = new Date() }) => {
  const title = titleFor(description);
  const details = [
    ['Reported', now.toISOString()],
    ['App version', info.version || '?'],
    ['Machine', `${os.hostname()} · ${os.platform()} ${os.release()} · ${os.arch()}`],
    ['Electron', info.electron || process.versions.electron || '?'],
    ['ffmpeg', info.ffmpeg ? 'found' : 'MISSING'],
    ['Plate reader', info.plateReader?.installed === false ? 'MISSING' : 'bundled'],
    ['Vehicle model', info.vehicleModel === false ? 'MISSING' : 'present'],
    ['Scale', scale ? `${scale.connected ? 'connected' : 'offline'} · ${scale.weight ?? '-'} ${scale.unit || ''} · ${scale.settled ? 'settled' : scale.motion ? 'moving' : 'settling'} · ${scale.loaded ? 'loaded' : 'clear'} · ${scale.frames || 0} frames${scale.lastError ? ` · last error: ${scale.lastError}` : ''}` : 'not set up']
  ];
  const detailTable = ['| | |', '|---|---|', ...details.map(([k, v]) => `| ${k} | ${redactCredentials(v)} |`)].join('\n');
  const entries = history.slice(0, HISTORY_ENTRIES).map(({ photo: _photo, ...e }) => JSON.stringify(e)).join('\n');
  const logText = redactCredentials(log);
  let logTail = logText.length > LOG_TAIL_CHARS ? logText.slice(logText.indexOf('\n', logText.length - LOG_TAIL_CHARS) + 1) : logText;

  const assemble = (tail) => [
    `## What happened`, '', String(description || '').trim() || '_(nothing typed)_', '',
    `## Station`, '', detailTable, '',
    `<details><summary>Settings (passwords removed)</summary>`, '', fence(JSON.stringify(redactSettings(settings), null, 2), 'json'), '', '</details>', '',
    `<details><summary>Last ${Math.min(history.length, HISTORY_ENTRIES)} weighings (newest first)</summary>`, '', fence(entries || '(none)'), '', '</details>', '',
    `<details><summary>Log (last ${tail.split('\n').filter(Boolean).length} lines)</summary>`, '', fence(tail || '(empty)'), '', '</details>', ''
  ].join('\n');

  let body = assemble(logTail);
  while (body.length > BODY_LIMIT && logTail.length > 1000) {
    logTail = logTail.slice(logTail.indexOf('\n', logTail.length - Math.floor(logTail.length * 0.7)) + 1);
    body = assemble(logTail);
  }
  if (body.length > BODY_LIMIT) body = `${body.slice(0, BODY_LIMIT - 20)}\n…(cut)`;
  return { title, body };
};

const explainStatus = (status, text) => {
  if (status === 401) return 'GitHub rejected the token. Check it on the Bug report tab.';
  if (status === 403) return 'GitHub refused: the token cannot open issues on this repository.';
  if (status === 404) return 'GitHub could not find the repository, or the token has no access to it.';
  if (status === 422) return `GitHub rejected the report: ${text.slice(0, 200)}`;
  return `GitHub answered ${status}: ${text.slice(0, 200)}`;
};

// Open the issue. Returns { number, url }.
const fileIssue = async ({ repo, token, title, body, labels = ['from-the-yard'], timeoutMs = 30000, fetchFn = globalThis.fetch }) => {
  const name = String(repo || '').trim().replace(/^https?:\/\/github\.com\//, '').replace(/\.git$/, '').replace(/\/+$/, '');
  if (!/^[\w.-]+\/[\w.-]+$/.test(name)) throw new Error('The repository must be written as owner/name');
  if (!String(token || '').trim()) throw new Error('No GitHub token is set. Add one on the Bug report tab.');
  if (!fetchFn) throw new Error('No HTTP client available');
  const controller = new globalThis.AbortController();
  const timer = setTimeout(() => controller.abort(), timeoutMs);
  let res;
  try {
    res = await fetchFn(`https://api.github.com/repos/${name}/issues`, {
      method: 'POST',
      signal: controller.signal,
      headers: {
        Authorization: `Bearer ${String(token).trim()}`,
        Accept: 'application/vnd.github+json',
        'Content-Type': 'application/json',
        'User-Agent': 'fgr-scale-station',
        'X-GitHub-Api-Version': '2022-11-28'
      },
      body: JSON.stringify({ title, body, labels })
    });
  } catch (err) {
    throw new Error(err.name === 'AbortError' ? `GitHub did not answer within ${Math.round(timeoutMs / 1000)} s` : `Could not reach GitHub: ${err.message}`);
  } finally {
    clearTimeout(timer);
  }
  const text = await res.text();
  // A label the repository does not have must not sink the report.
  if (res.status === 422 && labels.length) return fileIssue({ repo, token, title, body, labels: [], timeoutMs, fetchFn });
  if (!res.ok) throw new Error(explainStatus(res.status, text));
  let json = {};
  try { json = JSON.parse(text); } catch {}
  return { number: json.number, url: json.html_url };
};

// Send the report through the relay Worker (relay/ in the repository),
// which holds the GitHub token. Returns { number, url } like fileIssue.
const sendViaRelay = async ({ relayUrl, title, body, labels = ['from-the-yard'], timeoutMs = 30000, fetchFn = globalThis.fetch }) => {
  const base = String(relayUrl || '').trim().replace(/\/+$/, '');
  if (!/^https:\/\//.test(base)) throw new Error('This build has no bug report relay or GitHub token in it. Enter a token on the Bug report tab, or save the report to a file.');
  if (!fetchFn) throw new Error('No HTTP client available');
  const controller = new globalThis.AbortController();
  const timer = setTimeout(() => controller.abort(), timeoutMs);
  let res;
  try {
    res = await fetchFn(`${base}/report`, {
      method: 'POST', signal: controller.signal,
      headers: { 'Content-Type': 'application/json', 'User-Agent': 'fgr-scale-station' },
      body: JSON.stringify({ title, body, labels })
    });
  } catch (err) {
    throw new Error(err.name === 'AbortError' ? `The report relay did not answer within ${Math.round(timeoutMs / 1000)} s` : `Could not reach the report relay: ${err.message}`);
  } finally {
    clearTimeout(timer);
  }
  const text = await res.text();
  let json = {};
  try { json = JSON.parse(text); } catch {}
  if (!res.ok) throw new Error(json.error || `The report relay answered ${res.status}: ${text.slice(0, 200)}`);
  return { number: json.number, url: json.url };
};

// Whichever way is set up: a token on the tab goes straight to GitHub, else the relay.
const sendReport = async ({ support = {}, builtinRelayUrl = '', title, body, fetchFn }) => {
  if (String(support.githubToken || '').trim()) return fileIssue({ repo: support.repo, token: support.githubToken, title, body, fetchFn });
  return sendViaRelay({ relayUrl: support.relayUrl || builtinRelayUrl, title, body, fetchFn });
};

module.exports = { buildReport, fileIssue, sendViaRelay, sendReport, redactSettings, titleFor };
