// Where to download the Windows ffmpeg from, for scripts/fetch-ffmpeg.js and
// rust/scripts/fetch-ffmpeg.cjs.
//
// BtbN's "latest" release is deleted and made again by each daily autobuild,
// so for a few minutes a day its download answers 404 - which failed the
// v0.1.15 build. The newest dated autobuild has the same build under its own
// name (ffmpeg-N-<n>-g<hash>-win64-gpl.zip), so that is the fallback.
const https = require('https');

const LATEST = 'https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/ffmpeg-master-latest-win64-gpl.zip';
const RELEASES = 'https://api.github.com/repos/BtbN/FFmpeg-Builds/releases?per_page=5';
const DATED = /^ffmpeg-N-\d+-g[0-9a-f]+-win64-gpl\.zip$/;

const getJson = (url) => new Promise((resolve, reject) => {
  const headers = { 'User-Agent': 'fgr-scale-station', Accept: 'application/vnd.github+json' };
  // On the release runner, so a shared runner address is not rate-limited.
  const token = process.env.GH_TOKEN || process.env.GITHUB_TOKEN;
  if (token) headers.Authorization = `Bearer ${token}`;
  https.get(url, { headers }, (res) => {
    let body = '';
    res.on('data', (c) => { body += c; });
    res.on('end', () => {
      if (res.statusCode !== 200) return reject(new Error(`HTTP ${res.statusCode} for ${url}`));
      try { resolve(JSON.parse(body)); } catch (err) { reject(err); }
    });
  }).on('error', reject);
});

// The newest dated autobuild's Windows GPL zip.
const datedBuildUrl = async () => {
  for (const release of await getJson(RELEASES)) {
    const asset = (release.assets || []).find((a) => DATED.test(a.name));
    if (asset) return asset.browser_download_url;
  }
  throw new Error('no Windows build in BtbN\'s recent releases');
};

// Downloads with download(url, file): FFMPEG_WIN_URL if set, else "latest",
// else the newest dated build.
const downloadFfmpeg = async (download, file) => {
  if (process.env.FFMPEG_WIN_URL) return download(process.env.FFMPEG_WIN_URL, file);
  try {
    console.log(`Downloading ${LATEST} ...`);
    return await download(LATEST, file);
  } catch (err) {
    const url = await datedBuildUrl();
    console.log(`${err.message}; downloading ${url} instead ...`);
    return download(url, file);
  }
};

module.exports = { downloadFfmpeg, datedBuildUrl };
