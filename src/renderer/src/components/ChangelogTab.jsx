import React, { useEffect, useState } from 'react';

// What changed in each version, from the CHANGELOG.md shipped with the app.
// Rendered by hand: headings are versions, dashes are items. Nothing fancier
// is in that file, so nothing fancier is needed here.
export default function ChangelogTab({ draft, patch, info }) {
  const channel = draft?.updates?.channel === 'beta' ? 'beta' : 'stable';
  const [text, setText] = useState('');
  const [status, setStatus] = useState(null);
  const [checking, setChecking] = useState(false);
  useEffect(() => {
    window.station.changelog().then(setText);
    return window.station.updates.onStatus(setStatus);
  }, []);

  const check = async () => {
    setChecking(true);
    setStatus({ state: 'checking' });
    try { setStatus(await window.station.updates.check()); } finally { setChecking(false); }
  };
  const statusText = !status ? '' : {
    checking: 'Checking…',
    none: `You are up to date (${status.version || info.version}).`,
    downloading: `Downloading ${status.version || 'the update'}${status.percent ? ` · ${status.percent}%` : ''}…`,
    ready: `Version ${status.version} is downloaded. Use "Restart to update" in the header to install it.`,
    error: `Could not ${status.retryMinutes ? 'update' : 'check'}: ${status.message}${status.retryMinutes ? ` · trying again in ${status.retryMinutes} min` : ''}`,
    dev: status.message
  }[status.state] || '';

  const sections = [];
  for (const line of text.split(/\r?\n/)) {
    const version = line.match(/^##\s+(.+)$/);
    if (version) { sections.push({ version: version[1].trim(), items: [] }); continue; }
    const item = line.match(/^\s*[-*]\s+(.+)$/);
    if (item && sections.length) sections[sections.length - 1].items.push(item[1].trim());
  }

  return (
    <div className="space-y-5">
      <div className="rounded-md border border-emerald-900 bg-emerald-950/40 p-3 flex items-start justify-between gap-4 flex-wrap">
        <div>
          <h3 className="text-sm font-semibold text-emerald-200">Changelog</h3>
          <p className="text-xs text-emerald-300/80 mt-0.5">You are on version {info.version}. Updates download on their own when the PC has internet and install from the button in the header.</p>
          {statusText && <p className={`text-xs mt-1 ${status?.state === 'error' ? 'text-amber-300' : 'text-emerald-200'}`}>{statusText}</p>}
        </div>
        <button type="button" onClick={check} disabled={checking} className="px-3 py-1.5 text-sm rounded-md bg-emerald-700 hover:bg-emerald-600 text-white disabled:opacity-50 whitespace-nowrap">
          {checking ? 'Checking…' : 'Check for updates'}
        </button>
      </div>
      <section className="grid grid-cols-1 md:grid-cols-2 gap-5">
        <label className="block text-xs text-gray-400">Update channel
          <select className="w-full bg-gray-800 border border-gray-600 rounded-md px-2 py-1.5 text-sm text-gray-100 mt-1" value={channel} onChange={(e) => patch('updates', { channel: e.target.value })}>
            <option value="stable">Stable - releases only</option>
            <option value="beta">Beta - pre-releases too, for trying things first</option>
          </select>
          <span className="text-[11px] text-gray-500">{channel === 'beta'
            ? 'This PC takes beta builds as they are published, and any stable release newer than its beta. Switching back to Stable puts it on the stable release at the next check, even if that is older.'
            : 'This PC only takes stable releases. Pick Beta on the one PC that tries new builds first.'} Applies when you press Save.</span>
        </label>
        <div className="text-xs text-gray-400">All releases, with their installers
          <div className="mt-1"><button type="button" onClick={() => window.station.updates.openReleases()} className="px-3 py-1.5 text-sm rounded-md bg-gray-700 hover:bg-gray-600 text-gray-100">Open the releases page</button></div>
          <span className="text-[11px] text-gray-500">For installing a build by hand, such as a beta that is a different kind of app.</span>
        </div>
      </section>
      {!sections.length && <p className="text-sm text-gray-500">No changelog in this build.</p>}
      {sections.map((s) => (
        <section key={s.version} className="border-t border-gray-800 pt-3">
          <h4 className="text-sm font-semibold flex items-center gap-2">
            {s.version}
            {s.version === info.version && <span className="text-[10px] px-1.5 py-0.5 rounded bg-emerald-800 text-emerald-100">this version</span>}
          </h4>
          <ul className="mt-1.5 space-y-1 text-sm text-gray-300 list-disc pl-5">
            {s.items.map((item, i) => <li key={i}>{item}</li>)}
          </ul>
        </section>
      ))}
    </div>
  );
}
