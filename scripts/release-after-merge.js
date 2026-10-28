// After a pull request is merged into main: the next patch version of the
// Rust app in its three places (and the two lock files), and the PR's entry
// at the top of CHANGELOG.md, which the app shows on its Updates tab. Run by
// .github/workflows/release-after-merge.yml, which commits, tags and starts
// the release build; by hand, with a PR as `gh pr view N --json title,body`:
//   node scripts/release-after-merge.js pr.json     (prints the new version)
//
// The entry is the PR's "## Changelog" section if it has one: its dashes are
// the items, or its text is one item. Without one it is the PR's title. The
// changelog is read by the yard, so a PR can say there what changed for them
// in their words; the title is the fallback.
const fs = require('fs');
const path = require('path');

const root = path.join(__dirname, '..');

function nextVersion(version) {
  const m = /^(\d+)\.(\d+)\.(\d+)(-.+)?$/.exec(version);
  if (!m) throw new Error(`not a version: ${version}`);
  // 0.2.0-beta.3 is followed by 0.2.0 itself, 0.2.7 by 0.2.8.
  return m[4] ? `${m[1]}.${m[2]}.${m[3]}` : `${m[1]}.${m[2]}.${Number(m[3]) + 1}`;
}

function changelogItems({ title, body }) {
  const text = String(body || '').replace(/<!--[\s\S]*?-->/g, '').replace(/\r\n/g, '\n');
  const section = /^#{2,3}[ \t]*Changelog[^\n]*\n([\s\S]*?)(?=^#{1,3}[ \t]|$(?![\s\S]))/im.exec(text);
  const lines = section ? section[1].split('\n').map((l) => l.trim()).filter(Boolean) : [];
  if (lines.length) {
    const items = [];
    for (const line of lines) {
      const dash = /^[-*][ \t]+(.*)$/.exec(line);
      // A line without a dash carries on the item above it.
      if (dash) items.push(dash[1]);
      else if (items.length) items[items.length - 1] += ` ${line}`;
      else items.push(line);
    }
    return items;
  }
  const cleaned = String(title || '').replace(/\s*\((?:fixes\s+)?#\d+\)\s*$/i, '').trim();
  if (!cleaned) throw new Error('the PR has no title and no Changelog section');
  return [/[.!?]$/.test(cleaned) ? cleaned : `${cleaned}.`];
}

function addToChangelog(changelog, version, items) {
  const entry = `## ${version}\n\n${items.map((i) => `- ${i}`).join('\n')}\n\n`;
  const first = changelog.search(/^## /m);
  if (first === -1) return `${changelog.replace(/\n*$/, '\n\n')}${entry}`;
  if (new RegExp(`^## ${version.replace(/\./g, '\\.')}\\s*$`, 'm').test(changelog)) throw new Error(`CHANGELOG.md already has ${version}`);
  return changelog.slice(0, first) + entry + changelog.slice(first);
}

function setVersion(dir, version) {
  const edit = (file, fn) => {
    const p = path.join(dir, file);
    const before = fs.readFileSync(p, 'utf8');
    const after = fn(before);
    if (after === before) throw new Error(`no version changed in ${file}`);
    fs.writeFileSync(p, after);
  };
  // As JSON text, not parsed and written back, so nothing else in them moves.
  edit('rust/package.json', (t) => t.replace(/("version":\s*")[^"]+(")/, `$1${version}$2`));
  edit('rust/package-lock.json', (t) => t
    .replace(/^(\{\s*"name":\s*"[^"]+",\s*"version":\s*")[^"]+(")/, `$1${version}$2`)
    .replace(/("packages":\s*\{\s*"":\s*\{\s*"name":\s*"[^"]+",\s*"version":\s*")[^"]+(")/, `$1${version}$2`));
  edit('rust/src-tauri/tauri.conf.json', (t) => t.replace(/("version":\s*")[^"]+(")/, `$1${version}$2`));
  edit('rust/src-tauri/Cargo.toml', (t) => t.replace(/^(version\s*=\s*")[^"]+(")/m, `$1${version}$2`));
  edit('rust/src-tauri/Cargo.lock', (t) => t.replace(/(\nname = "falcon-scale-station"\nversion = ")[^"]+(")/, `$1${version}$2`));
}

if (require.main === module) {
  const pr = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
  const current = JSON.parse(fs.readFileSync(path.join(root, 'rust/src-tauri/tauri.conf.json'), 'utf8')).version;
  const version = nextVersion(current);
  const items = changelogItems(pr);
  setVersion(root, version);
  const changelog = path.join(root, 'CHANGELOG.md');
  fs.writeFileSync(changelog, addToChangelog(fs.readFileSync(changelog, 'utf8'), version, items));
  console.log(version);
}

module.exports = { nextVersion, changelogItems, addToChangelog, setVersion };
