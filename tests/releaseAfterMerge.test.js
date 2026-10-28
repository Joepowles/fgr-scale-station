// The version and changelog entry a merged PR is released with.
// Run with: node --test tests/releaseAfterMerge.test.js
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const os = require('os');
const path = require('path');
const { nextVersion, changelogItems, addToChangelog, setVersion } = require('../scripts/release-after-merge');

test('the next patch version, and a beta is followed by its release', () => {
  assert.equal(nextVersion('0.2.7'), '0.2.8');
  assert.equal(nextVersion('0.2.9'), '0.2.10');
  assert.equal(nextVersion('0.2.0-beta.3'), '0.2.0');
  assert.throws(() => nextVersion('soon'));
});

test('the Changelog section of the PR, item by item', () => {
  const body = [
    '<!-- claude-projects-attribution: {} -->',
    '_Requested by **Joe**_',
    '',
    '## Summary',
    'Fixes #31. The crash watchdog misread a shutdown.',
    '',
    '## Changelog',
    '',
    '- A PC shut down with the app open no longer sends a crash report',
    '  when it starts again.',
    '- Another thing.',
    '',
    '## Test plan',
    '- cargo test'
  ].join('\r\n');
  assert.deepEqual(changelogItems({ title: 'Crash reports: ignore shutdowns', body }), [
    'A PC shut down with the app open no longer sends a crash report when it starts again.',
    'Another thing.'
  ]);
});

test('a Changelog section without dashes is one item, and may end the body', () => {
  assert.deepEqual(changelogItems({ title: 't', body: '### Changelog\nThe weight is shown\nbigger.' }), ['The weight is shown bigger.']);
});

test('without a Changelog section, the title', () => {
  assert.deepEqual(changelogItems({ title: 'Camera picture lags on WC-OFFICE (#4)', body: '## Summary\nstuff' }), ['Camera picture lags on WC-OFFICE.']);
  assert.deepEqual(changelogItems({ title: 'Scale tab: fix typo.', body: null }), ['Scale tab: fix typo.']);
});

test('the entry goes above the newest version', () => {
  const before = '# Changelog\n\nNewest first.\n\n## 0.2.7\n\n- Old.\n';
  assert.equal(addToChangelog(before, '0.2.8', ['New.', 'Also new.']),
    '# Changelog\n\nNewest first.\n\n## 0.2.8\n\n- New.\n- Also new.\n\n## 0.2.7\n\n- Old.\n');
  assert.throws(() => addToChangelog(before, '0.2.7', ['x']), /already has 0\.2\.7/);
});

test('the version in all five files, and nothing else', () => {
  const repo = path.join(__dirname, '..');
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'release-'));
  const files = ['rust/package.json', 'rust/package-lock.json', 'rust/src-tauri/tauri.conf.json', 'rust/src-tauri/Cargo.toml', 'rust/src-tauri/Cargo.lock'];
  for (const f of files) {
    fs.mkdirSync(path.dirname(path.join(dir, f)), { recursive: true });
    fs.copyFileSync(path.join(repo, f), path.join(dir, f));
  }
  const current = JSON.parse(fs.readFileSync(path.join(dir, 'rust/package.json'), 'utf8')).version;
  setVersion(dir, '9.8.7');
  for (const f of files) {
    const before = fs.readFileSync(path.join(repo, f), 'utf8').split('\n');
    const after = fs.readFileSync(path.join(dir, f), 'utf8').split('\n');
    assert.equal(after.length, before.length, f);
    const changed = after.filter((line, i) => line !== before[i]);
    assert.ok(changed.length >= 1 && changed.length <= 2, `${f}: ${changed.length} lines changed`);
    for (const line of changed) assert.ok(line.includes('9.8.7') && !line.includes(current), `${f}: ${line}`);
  }
  assert.equal(JSON.parse(fs.readFileSync(path.join(dir, 'rust/package-lock.json'), 'utf8')).packages[''].version, '9.8.7');
  fs.rmSync(dir, { recursive: true, force: true });
});
