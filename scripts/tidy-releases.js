// Leave one release per tag: the one carrying the installer. Any other
// release on the same tag (electron-builder's parallel uploads used to make
// extras) has its assets moved over, then is deleted. Needs GH_TOKEN with
// contents:write; run by the release workflow, or by hand:
//   node scripts/tidy-releases.js v0.1.1
const { execFileSync } = require('child_process');
const fs = require('fs');
const os = require('os');
const path = require('path');

const tag = process.argv[2];
if (!tag) { console.error('usage: node scripts/tidy-releases.js <tag>'); process.exit(1); }
const repo = process.env.GITHUB_REPOSITORY || 'Joepowles/fgr-scale-station';
const gh = (args, opts = {}) => execFileSync('gh', args, { encoding: 'utf8', stdio: ['ignore', 'pipe', 'inherit'], ...opts });

const releases = JSON.parse(gh(['api', `repos/${repo}/releases?per_page=100`])).filter((r) => r.tag_name === tag);
if (releases.length <= 1) { console.log(`${tag}: ${releases.length} release, nothing to tidy`); process.exit(0); }
const keep = releases.find((r) => r.assets.some((a) => /\.exe$/i.test(a.name))) || releases[0];
const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'tidy-'));
for (const extra of releases) {
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
