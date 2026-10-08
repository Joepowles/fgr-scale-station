// The bug report relay: a Cloudflare Worker that takes a report from a yard
// PC and opens it as an issue on GitHub. The GitHub token lives here, as a
// Worker secret, so the installer carries nothing that could be pulled out
// of it, and a new yard sends reports with nothing set up.
//
//   POST /report   { title, body, labels? }   ->  201 { number, url }
//
// Anything else gets a short text answer. Ten reports a minute from one
// address is plenty; more than that is refused.

const TITLE_MAX = 256;
const BODY_MAX = 65536;
const REQUEST_MAX = 80 * 1024;

const json = (status, data) => new Response(JSON.stringify(data), { status, headers: { 'content-type': 'application/json' } });

const openIssue = async ({ repo, token, title, body, labels, fetchFn }) => {
  const post = (withLabels) => fetchFn(`https://api.github.com/repos/${repo}/issues`, {
    method: 'POST',
    headers: {
      Authorization: `Bearer ${token}`,
      Accept: 'application/vnd.github+json',
      'Content-Type': 'application/json',
      'User-Agent': 'fgr-scale-station-relay',
      'X-GitHub-Api-Version': '2022-11-28'
    },
    body: JSON.stringify({ title, body, labels: withLabels })
  });
  let res = await post(labels);
  // A label the repository lacks must not sink the report.
  if (res.status === 422 && labels.length) res = await post([]);
  return res;
};

export default {
  async fetch(request, env) {
    const url = new URL(request.url);
    if (request.method !== 'POST' || url.pathname !== '/report') {
      return new Response('Falcon Scale Station bug report relay. POST /report.', { status: request.method === 'GET' ? 200 : 404 });
    }
    if (env.REPORTS) {
      const key = request.headers.get('cf-connecting-ip') || 'unknown';
      const { success } = await env.REPORTS.limit({ key });
      if (!success) return json(429, { error: 'Too many reports from this address; try again in a minute.' });
    }
    const length = Number(request.headers.get('content-length') || 0);
    if (length > REQUEST_MAX) return json(413, { error: 'The report is too large.' });
    let report;
    try { report = await request.json(); } catch { return json(400, { error: 'The report is not JSON.' }); }
    const title = String(report?.title || '').trim().slice(0, TITLE_MAX);
    const body = String(report?.body || '').slice(0, BODY_MAX);
    if (!title || !body) return json(400, { error: 'A report needs a title and a body.' });
    const labels = Array.isArray(report.labels) ? report.labels.filter((l) => typeof l === 'string').slice(0, 5) : ['from-the-yard'];
    if (!env.GITHUB_TOKEN || !env.GITHUB_REPO) return json(500, { error: 'The relay has no GitHub token or repository set.' });

    const res = await openIssue({ repo: env.GITHUB_REPO, token: env.GITHUB_TOKEN, title, body, labels, fetchFn: env.fetchFn || fetch });
    const text = await res.text();
    if (!res.ok) {
      let message = text.slice(0, 200);
      try { message = JSON.parse(text).message || message; } catch {}
      return json(res.status === 401 || res.status === 403 || res.status === 404 ? 502 : res.status, { error: `GitHub answered ${res.status}: ${message}` });
    }
    let issue = {};
    try { issue = JSON.parse(text); } catch {}
    return json(201, { number: issue.number, url: issue.html_url });
  }
};
