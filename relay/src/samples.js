// Sample pictures from the yard, for working on truck recognition with real
// frames. A station with "Send sample pictures" on uploads each truck's
// frames and log as one .tar; they wait here in KV until fetched from the
// dev machine (relay/fetch-samples.mjs), and expire by themselves.
//
//   PUT    /samples/<station>/<visit>.tar   the app, while SAMPLES_UNTIL has not passed
//   GET    /samples                         list, with the SAMPLES_KEY secret as a bearer token
//   GET    /samples/<station>/<visit>.tar   one, with the key
//   DELETE /samples/<station>/<visit>.tar   one, with the key
//
// Uploads are open to anyone, as reports are, but only until the date in
// SAMPLES_UNTIL, only .tar, and at most 24 MB each: this is a temporary
// drop, not storage. Reading needs the key.

const SAMPLE_MAX = 24 * 1024 * 1024;
const KEEP_SECONDS = 30 * 24 * 3600;
const NAME = /^[A-Za-z0-9_-]{1,40}\/[A-Za-z0-9_.-]{1,80}\.tar$/;

const json = (status, data) => new Response(JSON.stringify(data), { status, headers: { 'content-type': 'application/json' } });

// Open through the end of the SAMPLES_UNTIL day (UTC).
export const open = (until, now = new Date()) => /^\d{4}-\d{2}-\d{2}$/.test(until || '') && now.toISOString().slice(0, 10) <= until;

const authorised = (request, env) => !!env.SAMPLES_KEY && request.headers.get('authorization') === `Bearer ${env.SAMPLES_KEY}`;

export async function samples(request, env, path) {
  const store = env.SAMPLE_STORE;
  if (!store) return json(503, { error: 'The relay has no sample store.' });
  const name = path.replace(/^\/samples\/?/, '');

  if (request.method === 'PUT') {
    if (!open(env.SAMPLES_UNTIL)) return json(410, { error: 'Sample uploads are closed.' });
    if (!NAME.test(name)) return json(400, { error: 'Not a sample name.' });
    if (env.SAMPLE_UPLOADS) {
      const { success } = await env.SAMPLE_UPLOADS.limit({ key: request.headers.get('cf-connecting-ip') || 'unknown' });
      if (!success) return json(429, { error: 'Too many uploads from this address; try again in a minute.' });
    }
    if (Number(request.headers.get('content-length') || 0) > SAMPLE_MAX) return json(413, { error: 'The sample is too large.' });
    const body = await request.arrayBuffer();
    if (!body.byteLength || body.byteLength > SAMPLE_MAX) return json(body.byteLength ? 413 : 400, { error: 'The sample is empty or too large.' });
    await store.put(name, body, { expirationTtl: KEEP_SECONDS, metadata: { size: body.byteLength, at: new Date().toISOString() } });
    return json(201, { name, size: body.byteLength });
  }

  if (!authorised(request, env)) return json(401, { error: 'Reading samples needs the key.' });
  if (request.method === 'GET' && !name) {
    const out = [];
    let cursor;
    do {
      const page = await store.list({ cursor });
      for (const k of page.keys) out.push({ name: k.name, size: k.metadata?.size, at: k.metadata?.at });
      cursor = page.list_complete ? undefined : page.cursor;
    } while (cursor);
    return json(200, { samples: out });
  }
  if (!NAME.test(name)) return json(400, { error: 'Not a sample name.' });
  if (request.method === 'GET') {
    const body = await store.get(name, 'arrayBuffer');
    return body ? new Response(body, { headers: { 'content-type': 'application/x-tar' } }) : json(404, { error: 'No such sample.' });
  }
  if (request.method === 'DELETE') {
    await store.delete(name);
    return new Response(null, { status: 204 });
  }
  return json(405, { error: 'Not allowed.' });
}
