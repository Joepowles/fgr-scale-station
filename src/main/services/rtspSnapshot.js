const { execFile } = require('child_process');
const path = require('path');
const fs = require('fs');
const os = require('os');
const { redactCredentials } = require('./redactCredentials');
const { ffmpegPath } = require('./ffmpegPath');

/**
 * Inject credentials into an rtsp:// URL if they are not already embedded.
 * e.g. rtspInjectCredentials('rtsp://192.168.1.1/stream1', 'admin', 'pass')
 *   => 'rtsp://admin:pass@192.168.1.1/stream1'
 */
const rtspInjectCredentials = (rtspUrl, user, pass) => {
  if (!user) return rtspUrl;
  // Don't inject if credentials already present (contains @)
  if (/@/.test(rtspUrl.replace(/^rtsp:\/\//i, ''))) return rtspUrl;
  const creds = `${encodeURIComponent(user)}:${encodeURIComponent(pass || '')}@`;
  return rtspUrl.replace(/^(rtsp:\/\/)/i, `$1${creds}`);
};

/**
 * Capture a single JPEG frame from an RTSP stream using ffmpeg.
 * Returns a Buffer containing the JPEG image data.
 *
 * @param {string} rtspUrl   - Full rtsp:// URL (credentials may be embedded or passed separately)
 * @param {string} [user]    - Optional username to inject if not in URL
 * @param {string} [pass]    - Optional password to inject if not in URL
 * @param {number} [timeoutMs=8000] - Hard timeout in milliseconds
 * @returns {Promise<Buffer>}
 */
const captureRtspSnapshot = (rtspUrl, user, pass, timeoutMs = 8000) => {
  const fullUrl = rtspInjectCredentials(rtspUrl, user, pass);
  const tmpFile = path.join(os.tmpdir(), `rtsp_snap_${Date.now()}_${Math.random().toString(36).slice(2)}.jpg`);

  return new Promise((resolve, reject) => {
    const args = [
      '-loglevel', 'error',        // suppress verbose ffmpeg output
      '-rtsp_transport', 'tcp',    // TCP is more reliable than UDP for LAN cameras
      // Keyframes only: a camera that starts its stream between keyframes
      // would otherwise give a grey smear for a first frame. And a 1 s probe:
      // ffmpeg's default probe is what waited longest, and keyframes-only made
      // it wait longer still (5.6 s a snapshot from the yard camera; 1.6 s
      // with both).
      '-skip_frame', 'nokey',
      '-analyzeduration', '1000000',
      '-i', fullUrl,
      '-vframes', '1',             // capture exactly one frame
      '-q:v', '2',                 // high quality JPEG (1=best, 31=worst)
      '-update', '1',              // write single file (not a sequence)
      tmpFile,
      '-y'                         // overwrite without prompt
    ];

    execFile(ffmpegPath(), args, { timeout: timeoutMs, windowsHide: true }, (err) => {
      if (err) {
        try { fs.unlinkSync(tmpFile); } catch {}
        // Improve error messages for common ffmpeg failures
        // execFile puts the whole command line — camera password and all —
        // into err.message, and this message is shown in the browser.
        const msg = redactCredentials(err.message || String(err));
        if (err.killed || msg.includes('ETIMEDOUT') || msg.includes('timeout')) {
          return reject(new Error(`RTSP stream timed out — no response from camera within ${timeoutMs / 1000}s`));
        }
        if (msg.includes('Connection refused') || msg.includes('ECONNREFUSED')) {
          return reject(new Error('RTSP connection refused — check the camera IP and port'));
        }
        if (msg.includes('401') || msg.includes('Unauthorized')) {
          return reject(new Error('RTSP authentication failed — check username and password'));
        }
        if (msg.includes('No such file') || msg.includes('ENOTFOUND')) {
          return reject(new Error(`RTSP host not found — check the camera address`));
        }
        return reject(new Error(`ffmpeg RTSP capture failed: ${msg.split('\n')[0]}`));
      }

      try {
        const buf = fs.readFileSync(tmpFile);
        try { fs.unlinkSync(tmpFile); } catch {}
        if (!buf || buf.length === 0) {
          return reject(new Error('ffmpeg returned an empty image file'));
        }
        resolve(buf);
      } catch (readErr) {
        reject(new Error(`Could not read captured frame: ${readErr.message}`));
      }
    });
  });
};

/**
 * Turn whatever is stored as a camera host (rtsp:// URL, http:// URL, or a
 * bare IP/hostname) into a full rtsp:// URL with credentials embedded.
 * Mirrors the logic of the live-view proxy route so both agree on the stream.
 */
const buildRtspUrl = (host, user, pass, defaultPath = '/stream2') => {
  const raw = String(host || '').trim();
  if (!raw) return null;
  if (/^rtsp:\/\//i.test(raw)) {
    const u = new URL(raw);
    if (!u.username && user) {
      u.username = encodeURIComponent(user);
      u.password = encodeURIComponent(pass || '');
    }
    if (!u.pathname || u.pathname === '/') u.pathname = defaultPath;
    return u.toString();
  }
  const u = new URL(/^https?:\/\//i.test(raw) ? raw : `http://${raw}`);
  const creds = user ? `${encodeURIComponent(user)}:${encodeURIComponent(pass || '')}@` : '';
  const rtspPath = (u.pathname && u.pathname !== '/') ? u.pathname : defaultPath;
  return `rtsp://${creds}${u.hostname}:554${rtspPath}`;
};

module.exports = { captureRtspSnapshot, rtspInjectCredentials, buildRtspUrl };
