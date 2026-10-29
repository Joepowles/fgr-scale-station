import React, { useEffect, useState } from 'react';
import { fmtWeight, fmtTime } from '../lib';

// Two weighings of one truck under an hour apart (pairing.rs): this one is
// the gross or the tare, and the line gives all three.
function PairLine({ pair, unit }) {
  const part = (key, label) => (
    <span className={pair.role === key ? 'text-gray-100' : ''}>{label} <span className="mono">{fmtWeight(pair[key], unit)}</span></span>
  );
  const how = pair.by === 'plate' ? `same plate ${pair.match}` : `same truck by ${pair.by}`;
  return (
    <div className="text-xs text-gray-400 flex flex-wrap gap-x-3">
      {part('gross', 'Gross')}
      {part('tare', 'Tare')}
      <span className="text-emerald-300">Net <span className="mono font-bold">{fmtWeight(pair.net, unit)}</span></span>
      <span className="text-gray-500">{how}, {pair.minutes} min apart{pair.shared ? ', tare shared with another load' : ''}</span>
    </div>
  );
}

// The weighings, newest first, sliding over the picture. Click one for the
// photo at full size.
// standalone: filling a window of its own, rather than over the picture.
export default function HistoryDrawer({ info, onClose, standalone = false }) {
  const [entries, setEntries] = useState([]);
  const [stats, setStats] = useState(null);
  // The id, not the entry: the entry is updated while the truck is still on
  // the deck (a better plate, a closer picture), and the preview follows it.
  const [openId, setOpenId] = useState(null);
  const open = entries.find((e) => e.id === openId) || null;

  const load = async () => {
    setEntries(await window.station.history.list({ limit: 300 }));
    setStats(await window.station.history.stats());
  };
  useEffect(() => { load(); return window.station.history.onEntry(() => load()); }, []);

  // A corrected photo keeps its file name, so the link carries when it last
  // changed: otherwise the window keeps showing the picture it loaded first.
  const photoUrl = (e) => `http://127.0.0.1:${info.serverPort}/photo/${encodeURIComponent(e.photo)}?v=${encodeURIComponent(e.photoAt || e.plate?.readAt || e.corrected?.at || '')}`;

  return (
    <div className={standalone ? 'absolute inset-0 bg-gray-950 flex flex-col' : 'absolute inset-y-0 right-0 w-full max-w-md bg-gray-950/95 border-l border-gray-800 flex flex-col shadow-2xl'}>
      <div className="flex items-center justify-between px-4 py-3 border-b border-gray-800">
        <div>
          <h2 className="font-semibold">History</h2>
          {stats && <p className="text-xs text-gray-500">{stats.entries} entries · {(stats.photoBytes / 1048576).toFixed(0)} MB of photos</p>}
        </div>
        <div className="flex gap-2">
          <button type="button" onClick={() => window.station.history.openFolder()} className="px-2 py-1 text-xs rounded bg-gray-800 hover:bg-gray-700">Open folder</button>
          {/* Off the picture into a window that can sit beside it (Rust app). */}
          {!standalone && window.station.window.openHistory && (
            <button type="button" onClick={async () => { await window.station.window.openHistory(); onClose(); }} className="px-2 py-1 text-xs rounded bg-gray-800 hover:bg-gray-700" title="Open the history in a window of its own, to move beside the picture or onto another screen">Pop out</button>
          )}
          {!standalone && <button type="button" onClick={onClose} className="px-2 py-1 text-xs rounded bg-gray-800 hover:bg-gray-700">Close</button>}
        </div>
      </div>
      <div className="flex-1 overflow-y-auto divide-y divide-gray-800">
        {!entries.length && <p className="p-4 text-sm text-gray-500">No weighings yet.</p>}
        {entries.map((e) => (
          <button key={e.id} type="button" onClick={() => setOpenId(e.id)} className="w-full text-left px-4 py-2.5 hover:bg-gray-900 flex items-center gap-3">
            {e.photo ? <img src={photoUrl(e)} alt="" className="w-20 h-12 object-cover rounded bg-black shrink-0" loading="lazy" /> : <div className="w-20 h-12 rounded bg-gray-900 shrink-0" />}
            <div className="min-w-0 flex-1">
              <div className="flex items-center gap-2">
                <span className="mono font-bold text-emerald-300">{fmtWeight(e.weight, e.unit)}</span>
                {e.plate?.text && <span className="mono text-xs font-bold px-1.5 py-0.5 rounded bg-gray-100 text-black">{e.plate.text}</span>}
              </div>
              <div className="text-xs text-gray-500">{fmtTime(e.at)}{e.vehicle?.label ? ` · ${e.vehicle.label}` : ''}{e.corrected ? ` · first read ${fmtWeight(e.corrected.from, e.unit)}` : ''}{e.note ? ` · ${e.note}` : ''}</div>
              {e.pair && <PairLine pair={e.pair} unit={e.unit} />}
            </div>
          </button>
        ))}
      </div>
      {open && (
        <div className="absolute inset-0 bg-black/95 flex flex-col" onClick={() => setOpenId(null)}>
          <div className="px-4 py-3 flex items-center justify-between text-sm">
            <span><span className="mono font-bold text-emerald-300">{fmtWeight(open.weight, open.unit)}</span>{open.plate?.text ? <> · <span className="mono font-bold">{open.plate.text}</span></> : ''} · {fmtTime(open.at)}{open.pair && <PairLine pair={open.pair} unit={open.unit} />}</span>
            <span className="text-gray-500">click to close</span>
          </div>
          {open.photo ? <img src={photoUrl(open)} alt="" className="flex-1 min-h-0 object-contain" /> : <p className="flex-1 flex items-center justify-center text-gray-500">No photo for this weighing</p>}
        </div>
      )}
    </div>
  );
}
