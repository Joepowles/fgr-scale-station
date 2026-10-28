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

// Sample pictures: a KV namespace as a Map.
const kv = () => {
  const m = new Map();
  return {
    m,
    put: async (k, v, o) => { m.set(k, { v, meta: o.metadata, ttl: o.expirationTtl }); },
    get: async (k) => m.get(k)?.v ?? null,
    delete: async (k) => { m.delete(k); },
    list: async () => ({ keys: [...m].map(([name, e]) => ({ name, metadata: e.meta })), list_complete: true })
  };
};
const sampleEnv = (store, extra = {}) => ({ SAMPLE_STORE: store, SAMPLES_UNTIL: '2999-01-01', SAMPLES_KEY: 'k3y', ...extra });
// The smallest plain tar the relay takes: a header with the ustar magic.
const tarBytes = (fill = 0) => {
  const b = new Uint8Array(1024).fill(fill, 0, 100);
  b.set([...'ustar'].map((c) => c.charCodeAt(0)), 257);
  return b;
};
const put = (name, bytes = tarBytes()) => new Request(`https://relay.example/samples/${name}`, { method: 'PUT', body: bytes });
const read = (path, method = 'GET', key = 'k3y') => new Request(`https://relay.example${path}`, { method, headers: key ? { authorization: `Bearer ${key}` } : {} });

test('takes a sample upload, and hands it back only with the key', async () => {
  const store = kv();
  const res = await worker.fetch(put('HOFFMAN-PC/20261029-101500-t12.tar'), sampleEnv(store));
  assert.equal(res.status, 201);
  assert.equal(store.m.get('HOFFMAN-PC/20261029-101500-t12.tar').ttl, 30 * 24 * 3600);
  assert.equal((await worker.fetch(read('/samples', 'GET', null), sampleEnv(store))).status, 401);
  assert.equal((await worker.fetch(read('/samples', 'GET', 'wrong'), sampleEnv(store))).status, 401);
  const list = await (await worker.fetch(read('/samples'), sampleEnv(store))).json();
  assert.deepEqual(list.samples.map((s) => [s.name, s.size]), [['HOFFMAN-PC/20261029-101500-t12.tar', 1024]]);
  const one = await worker.fetch(read('/samples/HOFFMAN-PC/20261029-101500-t12.tar'), sampleEnv(store));
  assert.deepEqual(new Uint8Array(await one.arrayBuffer()), tarBytes());
  assert.equal((await worker.fetch(read('/samples/HOFFMAN-PC/20261029-101500-t12.tar', 'DELETE'), sampleEnv(store))).status, 204);
  assert.equal(store.m.size, 0);
});

test('refuses samples once uploads close, with odd names, or with no key set', async () => {
  const store = kv();
  assert.equal((await worker.fetch(put('PC/a.tar'), sampleEnv(store, { SAMPLES_UNTIL: '2020-01-01' }))).status, 410);
  assert.equal((await worker.fetch(put('PC/a.tar'), sampleEnv(store, { SAMPLES_UNTIL: '' }))).status, 410);
  assert.equal((await worker.fetch(put('PC/../a.tar'), sampleEnv(store))).status, 400);
  assert.equal((await worker.fetch(put('PC/a.jpg'), sampleEnv(store))).status, 400);
  assert.equal((await worker.fetch(put('PC/a.tar', new Uint8Array()), sampleEnv(store))).status, 400);
  // Compressed, so it could unpack to far more than it weighs.
  assert.equal((await worker.fetch(put('PC/a.tar', new Uint8Array(1024).fill(0x1f)), sampleEnv(store))).status, 400);
  assert.equal((await worker.fetch(read('/samples'), sampleEnv(store, { SAMPLES_KEY: '' }))).status, 401);
  assert.equal(store.m.size, 0);
});

test('never replaces a sample already there', async () => {
  const store = kv();
  assert.equal((await worker.fetch(put('PC/v1.tar', tarBytes(1)), sampleEnv(store))).status, 201);
  assert.equal((await worker.fetch(put('PC/v1.tar', tarBytes(2)), sampleEnv(store))).status, 409);
  assert.deepEqual(store.m.get('PC/v1.tar').v, tarBytes(1).buffer);
});
