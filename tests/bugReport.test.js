// The bug report: passwords out, the log's tail and the last weighings in,
// under GitHub's body limit, filed with the token. Run with:
// node --test tests/bugReport.test.js
const test = require('node:test');
const assert = require('node:assert/strict');
const { buildReport, fileIssue, sendViaRelay, sendReport, redactSettings } = require('../src/main/services/bugReport');

const settings = {
  camera: { host: '10.0.0.5', username: 'admin', password: 'Egasi#687', streamUrl: 'rtsp://admin:Egasi%23687@10.0.0.5:554/live' },
  support: { githubToken: 'github_pat_abc', repo: 'Joepowles/fgr-scale-station' },
  scale: { minWeight: 4000 }
};

test('redacts passwords, tokens and URL credentials', () => {
  const r = redactSettings(settings);
  assert.equal(r.camera.password, '***');
  assert.equal(r.support.githubToken, '***');
  assert.equal(r.camera.streamUrl, 'rtsp://admin:***@10.0.0.5:554/live');
  assert.equal(r.camera.host, '10.0.0.5');
  assert.equal(r.scale.minWeight, 4000);
});

test('builds a report with the description first, the log tail and recent weighings, nothing secret', () => {
  const log = Array.from({ length: 5000 }, (_, i) => `2026-10-08T11:29:0${i % 10}.000Z weighed 32000 lb line ${i} rtsp://admin:Egasi%23687@10.0.0.5/live`).join('\n');
  const history = [{ id: 'a', weight: 32240, plate: { text: 'P1033192' }, photo: 'a.jpg' }, { id: 'b', weight: 32000, note: 'camera busy', photo: null }];
  const { title, body } = buildReport({
    description: 'Truck weighed eleven times\nIt sat on the scale at 32,000 lb.',
    info: { version: '0.1.8', ffmpeg: true }, settings, log, history, scale: { connected: true, weight: 32000, unit: 'lb', settled: true, loaded: true, frames: 10 }
  });
  assert.equal(title, 'Truck weighed eleven times');
  assert.equal(body.startsWith('## What happened\n\nTruck weighed eleven times\nIt sat on the scale at 32,000 lb.'), true);
  assert.equal(body.includes('Egasi'), false);
  assert.equal(body.includes('github_pat_abc'), false);
  assert.equal(body.includes('line 4999'), true);   // the end of the log is there
  assert.equal(body.includes('line 0 '), false);    // the start is not
  assert.equal(body.includes('"weight":32240'), true);
  assert.equal(body.includes('"photo"'), false);
  assert.equal(body.includes('App version | 0.1.8'), true);
  assert.equal(body.includes('connected · 32000 lb · settled · loaded'), true);
  assert.equal(body.length <= 60000, true);
});

test('a long title is cut and an empty description still reports', () => {
  const { title, body } = buildReport({ description: 'x'.repeat(200), settings: {}, log: '' });
  assert.equal(title.length, 78);
  assert.equal(buildReport({ description: '', settings: {} }).title, 'Problem at the scale');
  assert.equal(body.includes('(empty)'), true);
});

test('files the issue with the token and reads back its link', async () => {
  const calls = [];
  const fetchFn = async (url, opts) => { calls.push({ url, opts }); return { ok: true, status: 201, text: async () => JSON.stringify({ number: 12, html_url: 'https://github.com/Joepowles/fgr-scale-station/issues/12' }) }; };
  const r = await fileIssue({ repo: 'https://github.com/Joepowles/fgr-scale-station/', token: ' tok ', title: 'T', body: 'B', fetchFn });
  assert.deepEqual(r, { number: 12, url: 'https://github.com/Joepowles/fgr-scale-station/issues/12' });
  assert.equal(calls[0].url, 'https://api.github.com/repos/Joepowles/fgr-scale-station/issues');
  assert.equal(calls[0].opts.headers.Authorization, 'Bearer tok');
  assert.deepEqual(JSON.parse(calls[0].opts.body), { title: 'T', body: 'B', labels: ['from-the-yard'] });
});

test('explains a bad token, a missing repo and no token', async () => {
  const answer = (status) => async () => ({ ok: false, status, text: async () => '{"message":"Bad credentials"}' });
  await assert.rejects(fileIssue({ repo: 'a/b', token: 't', title: 'T', body: 'B', fetchFn: answer(401) }), /rejected the token/);
  await assert.rejects(fileIssue({ repo: 'a/b', token: 't', title: 'T', body: 'B', fetchFn: answer(404) }), /could not find the repository/);
  await assert.rejects(fileIssue({ repo: 'a/b', token: '', title: 'T', body: 'B', fetchFn: answer(201) }), /No GitHub token/);
  await assert.rejects(fileIssue({ repo: 'nonsense', token: 't', title: 'T', body: 'B', fetchFn: answer(201) }), /owner\/name/);
  await assert.rejects(fileIssue({ repo: 'a/b', token: 't', title: 'T', body: 'B', fetchFn: async () => { throw new Error('ENOTFOUND api.github.com'); } }), /Could not reach GitHub/);
});

test('a label the repository refuses is dropped and the report still goes', async () => {
  const bodies = [];
  const fetchFn = async (_url, opts) => {
    bodies.push(JSON.parse(opts.body));
    if (bodies.length === 1) return { ok: false, status: 422, text: async () => 'Validation Failed' };
    return { ok: true, status: 201, text: async () => JSON.stringify({ number: 3, html_url: 'u' }) };
  };
  const r = await fileIssue({ repo: 'a/b', token: 't', title: 'T', body: 'B', fetchFn });
  assert.equal(r.number, 3);
  assert.deepEqual(bodies.map((b) => b.labels), [['from-the-yard'], []]);
});

test('sends through the relay, and reads back the relay\'s answer or its error', async () => {
  const calls = [];
  const ok = async (url, opts) => { calls.push({ url, body: JSON.parse(opts.body) }); return { ok: true, status: 201, text: async () => JSON.stringify({ number: 9, url: 'https://github.com/Joepowles/fgr-scale-station/issues/9' }) }; };
  const r = await sendViaRelay({ relayUrl: 'https://scale-station-reports.example.workers.dev/', title: 'T', body: 'B', fetchFn: ok });
  assert.deepEqual(r, { number: 9, url: 'https://github.com/Joepowles/fgr-scale-station/issues/9' });
  assert.equal(calls[0].url, 'https://scale-station-reports.example.workers.dev/report');
  assert.deepEqual(calls[0].body, { title: 'T', body: 'B', labels: ['from-the-yard'] });
  const limited = async () => ({ ok: false, status: 429, text: async () => JSON.stringify({ error: 'Too many reports from this address; try again in a minute.' }) });
  await assert.rejects(sendViaRelay({ relayUrl: 'https://r.example', title: 'T', body: 'B', fetchFn: limited }), /Too many reports/);
  await assert.rejects(sendViaRelay({ relayUrl: '', title: 'T', body: 'B', fetchFn: ok }), /no bug report relay or GitHub token/);
  await assert.rejects(sendViaRelay({ relayUrl: 'https://r.example', title: 'T', body: 'B', fetchFn: async () => { throw new Error('ENOTFOUND'); } }), /Could not reach the report relay/);
});

test('a token on the tab goes straight to GitHub; otherwise the relay built into the app', async () => {
  const urls = [];
  const fetchFn = async (url) => { urls.push(url); return { ok: true, status: 201, text: async () => '{"number":1,"url":"u","html_url":"u"}' }; };
  await sendReport({ support: { githubToken: 't', repo: 'a/b' }, builtinRelayUrl: 'https://r.example', title: 'T', body: 'B', fetchFn });
  await sendReport({ support: { githubToken: '', repo: 'a/b' }, builtinRelayUrl: 'https://r.example', title: 'T', body: 'B', fetchFn });
  await sendReport({ support: { relayUrl: 'https://mine.example' }, builtinRelayUrl: 'https://r.example', title: 'T', body: 'B', fetchFn });
  assert.deepEqual(urls, ['https://api.github.com/repos/a/b/issues', 'https://r.example/report', 'https://mine.example/report']);
});
