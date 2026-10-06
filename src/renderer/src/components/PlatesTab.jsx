import React, { useState } from 'react';
import { CORNERS, btn } from '../lib';

export default function PlatesTab({ draft, patch, info }) {
  const p = draft.plates;
  const [reading, setReading] = useState(false);
  const [result, setResult] = useState(null);

  const readNow = async () => {
    setReading(true); setResult(null);
    const startedAt = Date.now();
    try { setResult({ ...(await window.station.plates.readNow()), ms: Date.now() - startedAt }); } catch (err) { setResult({ error: err.message, ms: Date.now() - startedAt }); } finally { setReading(false); }
  };
  const reader = result?.reader || info.plateReader;

  return (
    <div className="space-y-5">
      <div className="flex items-start justify-between gap-4 rounded-md border border-emerald-900 bg-emerald-950/40 p-3">
        <div>
          <h3 className="text-sm font-semibold text-emerald-200">Licence plate reading</h3>
          <p className="text-xs text-emerald-300/80 mt-0.5">Read on this PC with the bundled reader; nothing leaves the building. The plate is read off the full-resolution stream when a truck settles on the scale, shown over the picture while it is there, stamped on the photo and kept in the history.</p>
        </div>
        <label className="inline-flex items-center gap-2 text-sm whitespace-nowrap"><input type="checkbox" checked={!!p.enabled} onChange={(e) => patch('plates', { enabled: e.target.checked })} /> Read plates</label>
      </div>
      <section className="grid grid-cols-1 md:grid-cols-2 gap-5">
        <div className="space-y-3">
          <label className="block text-xs text-gray-400">Minimum read confidence: <span className="text-gray-200">{Math.round(p.minConfidence * 100)}%</span>
            <input type="range" min={10} max={99} value={Math.round(p.minConfidence * 100)} onChange={(e) => patch('plates', { minConfidence: Number(e.target.value) / 100 })} className="w-full" />
            <span className="text-[11px] text-gray-500">Judged on the least certain character. A read under this is not shown and the reader tries again.</span>
          </label>
          <div className="grid grid-cols-2 gap-3">
            <label className="block text-xs text-gray-400">Tries per truck<input type="number" min={1} max={30} className="w-full bg-gray-800 border border-gray-600 rounded-md px-2 py-1 text-sm mt-1" value={p.retryAttempts} onChange={(e) => patch('plates', { retryAttempts: Number(e.target.value) })} /></label>
            <label className="block text-xs text-gray-400">Seconds between tries<input type="number" min={1} max={30} className="w-full bg-gray-800 border border-gray-600 rounded-md px-2 py-1 text-sm mt-1" value={p.retrySeconds} onChange={(e) => patch('plates', { retrySeconds: Number(e.target.value) })} /></label>
          </div>
          <p className="text-[11px] text-gray-500">The reader keeps trying while the truck is on the deck until it is at least 90% sure, and puts the better read on the history entry.</p>
          <label className="flex items-center gap-2 text-xs text-gray-300"><input type="checkbox" checked={p.stampPhoto !== false} onChange={(e) => patch('plates', { stampPhoto: e.target.checked })} /> Stamp the plate on the saved photo</label>
          <div className="text-xs text-gray-400">Corner of the picture for the plate
            <div className="grid grid-cols-2 gap-1.5 mt-1 max-w-xs">
              {CORNERS.map((c) => (
                <button key={c.key} type="button" onClick={() => patch('plates', { overlayCorner: c.key })} className={`px-2 py-1.5 text-xs rounded border ${p.overlayCorner === c.key ? 'bg-gray-100 text-black border-gray-100' : 'bg-gray-800 text-gray-300 border-gray-600 hover:bg-gray-700'}`}>{c.label}</button>
              ))}
            </div>
          </div>
        </div>
        <div className="space-y-3">
          <h3 className="text-sm font-semibold">Try it</h3>
          <button type="button" onClick={readNow} disabled={reading || !draft.camera.streamUrl} className={btn}>{reading ? 'Reading…' : '🔎 Read a plate now'}</button>
          <p className="text-[11px] text-gray-500">Grabs a frame, finds the biggest vehicle in it and reads its plate from the full-resolution stream. Takes a few seconds. Uses the saved camera settings.</p>
          {result && (
            <p className={`text-xs ${result.plate ? 'text-emerald-300' : 'text-amber-300'}`}>
              {result.plate ? <>Read <span className="mono font-bold">{result.plate.text}</span> at {Math.round(result.plate.confidence * 100)}% in {(result.ms / 1000).toFixed(1)} s (frame {result.plate.frameWidth}×{result.plate.frameHeight})</> : `✗ ${result.error}`}
            </p>
          )}
          {result?.image && <img src={result.image} alt="" className="w-full rounded bg-black" />}
          {reader && !reader.installed && <p className="text-xs text-amber-400">The plate reader is not in this build ({reader.lastError || 'no Python'}).</p>}
          {reader?.lastError && reader.installed && <p className="text-xs text-amber-400">Plate reader: {reader.lastError}</p>}
        </div>
      </section>
    </div>
  );
}
