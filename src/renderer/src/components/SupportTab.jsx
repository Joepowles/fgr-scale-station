import React, { useState } from 'react';
import { btn, inputClass } from '../lib';

// Describe what went wrong and send it. The report carries the app's version,
// its settings with passwords removed, the last forty weighings and the tail
// of the log, so the problem can be looked at from anywhere. It goes up as an
// issue on GitHub with the token saved here; with no internet it can be saved
// to a file and sent along some other way.
export default function SupportTab({ draft, patch, info }) {
  const sup = draft.support || {};
  const [description, setDescription] = useState('');
  const [busy, setBusy] = useState('');
  const [result, setResult] = useState(null);

  const send = async () => {
    setBusy('send'); setResult(null);
    try {
      const issue = await window.station.support.send({ description, repo: sup.repo, token: sup.githubToken });
      setResult({ ok: true, text: `Sent as issue #${issue.number}.`, url: issue.url });
      setDescription('');
    } catch (err) {
      setResult({ ok: false, text: err.message.replace(/^Error invoking remote method '[^']+': Error: /, '') });
    } finally { setBusy(''); }
  };
  const save = async () => {
    setBusy('save'); setResult(null);
    try {
      const file = await window.station.support.save({ description });
      if (file) setResult({ ok: true, text: `Saved to ${file}. Send that file along.` });
    } catch (err) {
      setResult({ ok: false, text: err.message });
    } finally { setBusy(''); }
  };

  return (
    <div className="space-y-5">
      <div className="rounded-md border border-emerald-900 bg-emerald-950/40 p-3">
        <h3 className="text-sm font-semibold text-emerald-200">Bug report</h3>
        <p className="text-xs text-emerald-300/80 mt-0.5">Say what happened and when. The report goes with the app's version, its settings (passwords removed), the last 40 weighings and the recent log, so the problem can be looked at without anyone coming to this PC.</p>
      </div>
      <label className="block text-xs text-gray-400">What happened?
        <textarea className={`${inputClass} mt-1 min-h-[140px]`} value={description} onChange={(e) => setDescription(e.target.value)}
          placeholder={'For example: a truck pulled on at about 11:30 and the history shows it eleven times at 32,000 lb.'} />
      </label>
      <div className="flex items-center gap-2 flex-wrap">
        <button type="button" onClick={send} disabled={!!busy || !description.trim()} className="px-3 py-1.5 text-sm rounded-md bg-emerald-700 hover:bg-emerald-600 text-white disabled:opacity-50 flex items-center gap-2">
          {busy === 'send' && <div className="w-3.5 h-3.5 border-2 border-white/50 border-t-white rounded-full animate-spin" />}
          {busy === 'send' ? 'Sending…' : 'Send to GitHub'}
        </button>
        <button type="button" onClick={save} disabled={!!busy} className={btn}>{busy === 'save' ? 'Saving…' : 'Save report to a file'}</button>
        {result && (
          <span className={`text-xs ${result.ok ? 'text-emerald-300' : 'text-amber-300'}`}>
            {result.text}{result.url && <> <button type="button" onClick={() => window.station.support.open(result.url)} className="underline hover:text-white">Open it</button></>}
          </span>
        )}
      </div>
      <section className="border-t border-gray-800 pt-4 grid grid-cols-1 md:grid-cols-2 gap-5">
        <label className="block text-xs text-gray-400">GitHub token
          <input type="password" className={`${inputClass} mono mt-1`} value={sup.githubToken || ''} onChange={(e) => patch('support', { githubToken: e.target.value.trim() })} placeholder="github_pat_…" autoComplete="off" />
          <span className="text-[11px] text-gray-500">A fine-grained token for the repository below with Issues set to read and write, nothing else. Saved with the other settings when you press Save.</span>
        </label>
        <label className="block text-xs text-gray-400">Repository
          <input className={`${inputClass} mono mt-1`} value={sup.repo || ''} onChange={(e) => patch('support', { repo: e.target.value.trim() })} placeholder="owner/name" />
          <span className="text-[11px] text-gray-500">Where the reports go. Version {info.version} of this app came from here.</span>
        </label>
      </section>
    </div>
  );
}
