// Download a static Windows ffmpeg into resources/bin for packaging.
//
// BtbN's GPL builds are the standard static Windows binaries. Run on the
// release runner (or any machine) before electron-builder; the folder is
// gitignored. Usage: node scripts/fetch-ffmpeg.js
const fs = require('fs');
const path = require('path');
const https = require('https');
const { execSync } = require('child_process');

const FFMPEG_URL = process.env.FFMPEG_WIN_URL
  || 'https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/ffmpeg-master-latest-win64-gpl.zip';
const dest = path.join(__dirname, '..', 'resources', 'bin');
const zipPath = path.join(dest, 'ffmpeg.zip');

const download = (url, file, redirects = 0) => new Promise((resolve, reject) => {
  https.get(url, { headers: { 'User-Agent': 'fgr-scale-station' } }, (res) => {
    if ([301, 302, 303, 307, 308].includes(res.statusCode) && res.headers.location && redirects < 5) {
      res.resume();
      return resolve(download(new URL(res.headers.location, url).toString(), file, redirects + 1));
    }
    if (res.statusCode !== 200) return reject(new Error(`HTTP ${res.statusCode} for ${url}`));
    const out = fs.createWriteStream(file);
    res.pipe(out);
    out.on('finish', () => out.close(resolve));
    out.on('error', reject);
  }).on('error', reject);
});

(async () => {
  fs.mkdirSync(dest, { recursive: true });
  const target = path.join(dest, 'ffmpeg.exe');
  if (fs.existsSync(target) && fs.statSync(target).size > 10 * 1024 * 1024) {
    console.log(`ffmpeg already present at ${target}`);
    return;
  }
  console.log(`Downloading ${FFMPEG_URL} ...`);
  await download(FFMPEG_URL, zipPath);
  const extractDir = path.join(dest, 'extract');
  fs.rmSync(extractDir, { recursive: true, force: true });
  if (process.platform === 'win32') {
    execSync(`powershell -NoProfile -Command "Expand-Archive -LiteralPath '${zipPath}' -DestinationPath '${extractDir}' -Force"`, { stdio: 'inherit' });
  } else {
    execSync(`unzip -q -o "${zipPath}" -d "${extractDir}"`, { stdio: 'inherit' });
  }
  const findExe = (dir) => {
    for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
      const full = path.join(dir, entry.name);
      if (entry.isDirectory()) { const found = findExe(full); if (found) return found; }
      else if (entry.name.toLowerCase() === 'ffmpeg.exe') return full;
    }
    return null;
  };
  const exe = findExe(extractDir);
  if (!exe) throw new Error('ffmpeg.exe not found in the archive');
  fs.copyFileSync(exe, target);
  fs.rmSync(extractDir, { recursive: true, force: true });
  fs.rmSync(zipPath, { force: true });
  console.log(`ffmpeg ready: ${target} (${Math.round(fs.statSync(target).size / 1048576)} MB)`);
})().catch((err) => { console.error(err.message); process.exit(1); });
