// After `tauri build`: put the installer where the release workflow uploads
// it, named as the Electron app's were, with the update feed file beside it.
//
// The feed is electron-builder's format (version, files, path, sha512), as
// latest.yml for a release or beta.yml for a pre-release (0.2.0-beta.1).
// Both apps' updaters read it, which is how a PC on the Electron app moves to
// this one; src-tauri/src/updater.rs reads it here.
// Usage: node scripts/write-update-feed.cjs [out-dir]   (default: dist-release)
const crypto = require('crypto');
const fs = require('fs');
const path = require('path');

const root = path.join(__dirname, '..');
const version = JSON.parse(fs.readFileSync(path.join(root, 'src-tauri', 'tauri.conf.json'), 'utf8')).version;
const outDir = path.resolve(process.argv[2] || path.join(root, 'dist-release'));
const nsisDir = path.join(root, 'src-tauri', 'target', 'release', 'bundle', 'nsis');

const built = fs.readdirSync(nsisDir).filter((f) => f.endsWith('-setup.exe') && f.includes(`_${version}_`));
if (built.length !== 1) throw new Error(`expected one installer for ${version} in ${nsisDir}, found: ${built.join(', ') || 'none'}`);

fs.mkdirSync(outDir, { recursive: true });
const file = `Falcon-Scale-Station-Setup-${version}.exe`;
const target = path.join(outDir, file);
fs.copyFileSync(path.join(nsisDir, built[0]), target);
const bytes = fs.readFileSync(target);
const sha512 = crypto.createHash('sha512').update(bytes).digest('base64');
const channel = version.includes('-') ? 'beta' : 'latest';
const feed = [
  `version: ${version}`,
  'files:',
  `  - url: ${file}`,
  `    sha512: ${sha512}`,
  `    size: ${bytes.length}`,
  `path: ${file}`,
  `sha512: ${sha512}`,
  `releaseDate: '${new Date().toISOString()}'`,
  ''
].join('\n');
fs.writeFileSync(path.join(outDir, `${channel}.yml`), feed);
console.log(`${file} (${Math.round(bytes.length / 1048576)} MB) and ${channel}.yml in ${outDir}`);
