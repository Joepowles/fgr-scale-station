import React, { useEffect, useState } from 'react';
import { btn, inputClass, timeAgo } from '../lib';

export default function HistoryTab({ draft, patch, info }) {
  const h = draft.history;
  const [stats, setStats] = useState(null);
  const refresh = () => window.station.history.stats().then(setStats);
  useEffect(() => { refresh(); }, []);

  const choose = async () => {
    const folder = await window.station.history.chooseFolder();
    if (folder) patch('history', { folder });
  };

  return (
    <div className="space-y-5">
      <div className="rounded-md border border-emerald-900 bg-emerald-950/40 p-3">
        <h3 className="text-sm font-semibold text-emerald-200">History</h3>
        <p className="text-xs text-emerald-300/80 mt-0.5">Every weighing goes into a plain log file, one line each, with its photo beside it. Both live in a folder you choose and are dropped after the number of days you choose. The folder can be opened in Explorer or pointed at a network share.</p>
      </div>
      <section className="grid grid-cols-1 md:grid-cols-2 gap-5">
        <div className="space-y-3">
          <label className="block text-xs text-gray-400">Keep for
            <div className="flex items-center gap-2 mt-1">
              <input type="number" min={1} max={3650} className="w-28 bg-gray-800 border border-gray-600 rounded-md px-2 py-1 text-sm text-gray-100" value={h.retentionDays} onChange={(e) => patch('history', { retentionDays: Number(e.target.value) })} />
              <span className="text-xs text-gray-500">days</span>
              {[7, 30, 90, 365].map((d) => <button key={d} type="button" onClick={() => patch('history', { retentionDays: d })} className="px-2 py-0.5 text-[11px] rounded bg-gray-800 hover:bg-gray-700">{d === 7 ? '1 week' : d === 30 ? '1 month' : d === 90 ? '3 months' : '1 year'}</button>)}
            </div>
            <span className="text-[11px] text-gray-500">Older entries and their photos are removed once an hour.</span>
          </label>
          <label className="flex items-center gap-2 text-xs text-gray-300"><input type="checkbox" checked={h.savePhotos !== false} onChange={(e) => patch('history', { savePhotos: e.target.checked })} /> Save a photo of each weighing</label>
          <label className="flex items-center gap-2 text-xs text-gray-300"><input type="checkbox" checked={h.saveLog !== false} onChange={(e) => patch('history', { saveLog: e.target.checked })} /> Keep the log file (history.jsonl)</label>
        </div>
        <div className="space-y-3">
          <label className="block text-xs text-gray-400">Folder
            <div className="flex gap-2 mt-1">
              <input className={`${inputClass} mono`} value={h.folder} placeholder={`${info.userData}\\history`} onChange={(e) => patch('history', { folder: e.target.value })} />
              <button type="button" onClick={choose} className={`${btn} whitespace-nowrap`}>Choose…</button>
            </div>
            <span className="text-[11px] text-gray-500">Leave blank to keep it under this app's data folder. Changing it does not move what is already saved.</span>
          </label>
          <div className="flex gap-2">
            <button type="button" onClick={() => window.station.history.openFolder()} className={btn}>Open folder</button>
            <button type="button" onClick={async () => { await window.station.history.prune(); refresh(); }} className={btn}>Clean up old entries now</button>
          </div>
          {stats && (
            <div className="text-xs text-gray-400 space-y-0.5">
              <div>{stats.entries} entries, {(stats.photoBytes / 1048576).toFixed(0)} MB of photos</div>
              <div>Oldest {stats.oldest ? timeAgo(stats.oldest) : '—'} · newest {stats.newest ? timeAgo(stats.newest) : '—'}</div>
              <div className="mono truncate" title={stats.folder}>{stats.folder}</div>
            </div>
          )}
        </div>
      </section>
    </div>
  );
}
