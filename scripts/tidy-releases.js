// Leave one release per tag: the one carrying the installer. Any other
// release on the same tag (electron-builder's parallel uploads used to make
// extras) has its assets moved over, then is deleted. With --keep N, older
// releases beyond the newest N are deleted too (their git tags stay). Needs
// GH_TOKEN with contents:write; run by the release workflow, or by hand:
//   node scripts/tidy-releases.js v0.1.1 --keep 5
const { execFileSync } = require('child_process');
const fs = require('fs');
const os = require('os');
const path = require('path');

const tag = process.argv[2];
const keepArg = process.argv.indexOf('--keep');
const keepCount = keepArg !== -1 ? Number(process.argv[keepArg + 1]) || 0 : 0;
if (!tag) { console.error('usage: node scripts/tidy-releases.js <tag> [--keep N]'); process.exit(1); }
const repo = process.env.GITHUB_REPOSITORY || 'Joepowles/fgr-scale-station';
const gh = (args, opts = {}) => execFileSync('gh', args, { encoding: 'utf8', stdio: ['ignore', 'pipe', 'inherit'], ...opts });

const all = JSON.parse(gh(['api', `repos/${repo}/releases?per_page=100`]));
const releases = all.filter((r) => r.tag_name === tag);
if (releases.length <= 1) console.log(`${tag}: ${releases.length} release, nothing to merge`);
const keep = releases.find((r) => r.assets.some((a) => /\.exe$/i.test(a.name))) || releases[0];
const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'tidy-'));
for (const extra of releases.length > 1 ? releases : []) {
  if (extra.id === keep.id) continue;
  for (const asset of extra.assets) {
    if (keep.assets.some((a) => a.name === asset.name)) continue;
    const file = path.join(tmp, asset.name);
    const data = execFileSync('gh', ['api', '-H', 'Accept: application/octet-stream', `repos/${repo}/releases/assets/${asset.id}`], { maxBuffer: 1024 * 1024 * 1024 });
    fs.writeFileSync(file, data);
    gh(['release', 'upload', tag, file, '--repo', repo, '--clobber']);
    console.log(`${tag}: moved ${asset.name} to release ${keep.id}`);
  }
  gh(['api', '-X', 'DELETE', `repos/${repo}/releases/${extra.id}`]);
  console.log(`${tag}: deleted duplicate release ${extra.id}`);
}

// Oldest beyond the newest N go, so the yard's auto-update history does not
// pile up installers forever.
if (keepCount > 0) {
  const byTag = new Map();
  for (const r of all.filter((r) => !r.draft).sort((a, b) => new Date(b.created_at) - new Date(a.created_at))) {
    if (!byTag.has(r.tag_name)) byTag.set(r.tag_name, r);
  }
  const ordered = [...byTag.values()];
  for (const old of ordered.slice(keepCount)) {
    gh(['api', '-X', 'DELETE', `repos/${repo}/releases/${old.id}`]);
    console.log(`removed old release ${old.tag_name} (keeping the newest ${keepCount})`);
  }
}
