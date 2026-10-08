// The relay Worker, driven directly: a report in, an issue out, with the
// token from the environment and the limits enforced. Run with:
// node --test tests/relay.test.mjs
import test from 'node:test';
import assert from 'node:assert/strict';
import worker from '../relay/src/index.js';

const github = (answers) => {
  const calls = [];
  const fetchFn = async (url, opts) => {
    calls.push({ url, body: JSON.parse(opts.body), auth: opts.headers.Authorization });
    const a = answers[Math.min(calls.length - 1, answers.length - 1)];
    return { ok: a.status < 300, status: a.status, text: async () => JSON.stringify(a.body) };
  };
  return { calls, fetchFn };
};
const env = (fetchFn, extra = {}) => ({ GITHUB_TOKEN: 'tok', GITHUB_REPO: 'Joepowles/fgr-scale-station', fetchFn, ...extra });
const post = (data, headers = {}) => new Request('https://relay.example/report', { method: 'POST', headers: { 'content-type': 'application/json', ...headers }, body: JSON.stringify(data) });

test('opens the issue with the token the Worker holds', async () => {
  const gh = github([{ status: 201, body: { number: 7, html_url: 'https://github.com/Joepowles/fgr-scale-station/issues/7' } }]);
  const res = await worker.fetch(post({ title: 'Truck weighed twice', body: 'details' }), env(gh.fetchFn));
  assert.equal(res.status, 201);
  assert.deepEqual(await res.json(), { number: 7, url: 'https://github.com/Joepowles/fgr-scale-station/issues/7' });
  assert.equal(gh.calls[0].url, 'https://api.github.com/repos/Joepowles/fgr-scale-station/issues');
  assert.equal(gh.calls[0].auth, 'Bearer tok');
  assert.deepEqual(gh.calls[0].body.labels, ['from-the-yard']);
});

test('drops the label when GitHub refuses it', async () => {
  const gh = github([{ status: 422, body: { message: 'Validation Failed' } }, { status: 201, body: { number: 8, html_url: 'u' } }]);
  const res = await worker.fetch(post({ title: 'T', body: 'B' }), env(gh.fetchFn));
  assert.equal(res.status, 201);
  assert.deepEqual(gh.calls.map((c) => c.body.labels), [['from-the-yard'], []]);
});

test('refuses bad reports, too many reports, and other paths', async () => {
  const gh = github([{ status: 201, body: {} }]);
  assert.equal((await worker.fetch(post({ title: '', body: 'B' }), env(gh.fetchFn))).status, 400);
  assert.equal((await worker.fetch(new Request('https://relay.example/report', { method: 'POST', body: 'not json' }), env(gh.fetchFn))).status, 400);
  assert.equal((await worker.fetch(post({ title: 'T', body: 'B' }, { 'content-length': String(200 * 1024) }), env(gh.fetchFn))).status, 413);
  assert.equal((await worker.fetch(new Request('https://relay.example/'), env(gh.fetchFn))).status, 200);
  assert.equal((await worker.fetch(new Request('https://relay.example/other', { method: 'POST' }), env(gh.fetchFn))).status, 404);
  const limited = env(gh.fetchFn, { REPORTS: { limit: async () => ({ success: false }) } });
  assert.equal((await worker.fetch(post({ title: 'T', body: 'B' }), limited)).status, 429);
  assert.equal(gh.calls.length, 0);
});

test('a GitHub token problem is reported as the relay being broken, not the report', async () => {
  const gh = github([{ status: 401, body: { message: 'Bad credentials' } }]);
  const res = await worker.fetch(post({ title: 'T', body: 'B' }), env(gh.fetchFn));
  assert.equal(res.status, 502);
  assert.match((await res.json()).error, /GitHub answered 401: Bad credentials/);
});
