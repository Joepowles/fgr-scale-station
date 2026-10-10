// Put ffmpeg where Tauri wants a sidecar: src-tauri/binaries/ffmpeg-<target
// triple>[.exe]. Tauri copies it beside the app's executable at build time
// (bundle.externalBin in tauri.conf.json), where camera/ffmpeg.rs finds it.
//
// For Windows, BtbN's static GPL build, as the Electron app shipped. On
// Linux (development), the ffmpeg on the PATH is copied in.
// Usage: node scripts/fetch-ffmpeg.cjs [target-triple]
const fs = require('fs');
const path = require('path');
const https = require('https');
const { execSync } = require('child_process');

const FFMPEG_WIN_URL = process.env.FFMPEG_WIN_URL
  || 'https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/ffmpeg-master-latest-win64-gpl.zip';
const triple = process.argv[2] || execSync('rustc -vV', { encoding: 'utf8' }).match(/^host: (.+)$/m)[1].trim();
const windows = triple.includes('windows');
const dest = path.join(__dirname, '..', 'src-tauri', 'binaries');
const target = path.join(dest, `ffmpeg-${triple}${windows ? '.exe' : ''}`);

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

const findFile = (dir, name) => {
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) { const found = findFile(full, name); if (found) return found; }
    else if (entry.name.toLowerCase() === name) return full;
  }
  return null;
};

(async () => {
  fs.mkdirSync(dest, { recursive: true });
  if (fs.existsSync(target) && fs.statSync(target).size > 1024 * 1024) {
    console.log(`ffmpeg already present at ${target}`);
    return;
  }
  if (!windows) {
    const onPath = execSync('command -v ffmpeg', { encoding: 'utf8', shell: '/bin/sh' }).trim();
    fs.copyFileSync(onPath, target);
    fs.chmodSync(target, 0o755);
    console.log(`ffmpeg ready: ${target} (copied from ${onPath})`);
    return;
  }
  const zipPath = path.join(dest, 'ffmpeg.zip');
  const extractDir = path.join(dest, 'extract');
  console.log(`Downloading ${FFMPEG_WIN_URL} ...`);
  await download(FFMPEG_WIN_URL, zipPath);
  fs.rmSync(extractDir, { recursive: true, force: true });
  if (process.platform === 'win32') {
    execSync(`powershell -NoProfile -Command "Expand-Archive -LiteralPath '${zipPath}' -DestinationPath '${extractDir}' -Force"`, { stdio: 'inherit' });
  } else {
    execSync(`unzip -q -o "${zipPath}" -d "${extractDir}"`, { stdio: 'inherit' });
  }
  const exe = findFile(extractDir, 'ffmpeg.exe');
  if (!exe) throw new Error('ffmpeg.exe not found in the archive');
  fs.copyFileSync(exe, target);
  fs.rmSync(extractDir, { recursive: true, force: true });
  fs.rmSync(zipPath, { force: true });
  console.log(`ffmpeg ready: ${target} (${Math.round(fs.statSync(target).size / 1048576)} MB)`);
})().catch((err) => { console.error(err.message); process.exit(1); });
