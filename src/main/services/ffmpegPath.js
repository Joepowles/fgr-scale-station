// The bundled ffmpeg when it is there (installed builds), otherwise whatever
// is on the PATH (development on a box with ffmpeg installed).
const fs = require('fs');
const { execSync } = require('child_process');
const paths = require('../paths');

let resolved = null;
const ffmpegPath = () => {
  if (resolved) return resolved;
  const bundled = paths.ffmpegExe();
  if (fs.existsSync(bundled)) { resolved = bundled; return resolved; }
  try {
    const cmd = process.platform === 'win32' ? 'where ffmpeg' : 'which ffmpeg';
    const found = execSync(cmd, { encoding: 'utf8', stdio: ['ignore', 'pipe', 'ignore'] }).trim().split(/\r?\n/)[0];
    resolved = found || 'ffmpeg';
  } catch {
    resolved = 'ffmpeg';
  }
  return resolved;
};

const ffmpegAvailable = () => {
  const p = ffmpegPath();
  if (fs.existsSync(p)) return true;
  try { execSync(`"${p}" -version`, { stdio: 'ignore' }); return true; } catch { return false; }
};

module.exports = { ffmpegPath, ffmpegAvailable };
