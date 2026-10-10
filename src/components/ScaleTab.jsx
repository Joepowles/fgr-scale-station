import React, { useEffect, useState } from 'react';
import { CORNERS, fmtWeight, timeAgo, inputClass, btn } from '../lib';

export default function ScaleTab({ draft, patch }) {
  const s = draft.scale;
  const [finding, setFinding] = useState(false);
  const [progress, setProgress] = useState(null);
  const [found, setFound] = useState(null);
  const [testing, setTesting] = useState(false);
  const [test, setTest] = useState(null);
  const [live, setLive] = useState(null);

  useEffect(() => {
    window.station.scale.status().then(setLive);
    const offs = [window.station.scale.onStatus(setLive), window.station.scale.onDiscoverProgress(setProgress)];
    return () => offs.forEach((off) => off());
  }, []);

  const discover = async () => {
    setFinding(true); setFound(null); setProgress(null);
    try { setFound(await window.station.scale.discover()); } finally { setFinding(false); setProgress(null); }
  };
  const runTest = async () => {
    setTesting(true); setTest(null);
    try { setTest(await window.station.scale.test({ host: s.host, port: s.port, seconds: 3 })); } catch (err) { setTest({ ok: false, error: err.message }); } finally { setTesting(false); }
  };
  const num = (key, label, hint, { min = 0, step = 1, suffix } = {}) => (
    <label className="block text-xs text-gray-400">{label}
      <div className="flex items-center gap-2 mt-1">
        <input type="number" min={min} step={step} className="w-32 bg-gray-800 border border-gray-600 rounded-md px-2 py-1 text-sm text-gray-100" value={s[key]} onChange={(e) => patch('scale', { [key]: e.target.value === '' ? '' : Number(e.target.value) })} />
        <span className="text-xs text-gray-500">{suffix || s.unit}</span>
      </div>
      {hint && <span className="text-[11px] text-gray-500">{hint}</span>}
    </label>
  );

  return (
    <div className="space-y-5">
      <div className="flex items-start justify-between gap-4 rounded-md border border-emerald-900 bg-emerald-950/40 p-3">
        <div>
          <h3 className="text-sm font-semibold text-emerald-200">Truck scale</h3>
          <p className="text-xs text-emerald-300/80 mt-0.5">Reads the indicator's serial output over the network through the device server it is wired to, shows the live weight, and photographs each truck when its weight settles.</p>
        </div>
        <label className="inline-flex items-center gap-2 text-sm whitespace-nowrap"><input type="checkbox" checked={!!s.enabled} onChange={(e) => patch('scale', { enabled: e.target.checked })} /> Read the scale</label>
      </div>
      <section className="grid grid-cols-1 md:grid-cols-2 gap-5">
        <div className="space-y-3">
          <h3 className="text-sm font-semibold">Serial connection</h3>
          <p className="text-[11px] text-gray-500">A Moxa NPort in Real COM mode hands the data to any client on port 950; in TCP Server mode it is port 4001. Either way the scale software keeps its own connection.</p>
          <div className="grid grid-cols-3 gap-2">
            <label className="block text-xs text-gray-400 col-span-2">Device server address<input className={`${inputClass} mono`} value={s.host} placeholder="192.168.1.9" onChange={(e) => patch('scale', { host: e.target.value })} /></label>
            <label className="block text-xs text-gray-400">TCP port<input type="number" className={inputClass} value={s.port} onChange={(e) => patch('scale', { port: Number(e.target.value) })} /></label>
          </div>
          <div className="flex gap-2 flex-wrap">
            <button type="button" onClick={discover} disabled={finding} className={btn}>{finding ? (progress ? `Scanning… ${Math.round((progress.done / progress.total) * 100)}%` : 'Searching…') : 'Find the device server'}</button>
            <button type="button" onClick={runTest} disabled={testing || !s.host} className={btn}>{testing ? 'Listening for 3 s…' : '🔌 Test connection'}</button>
          </div>
          {found && (
            <div className="border border-gray-700 rounded-md divide-y divide-gray-800 text-xs">
              {!found.length && <p className="p-2 text-gray-500">Nothing on the LAN answered on the serial data ports. Is the NPort on this network and powered?</p>}
              {found.map((d) => (
                <button key={d.host} type="button" onClick={() => patch('scale', { host: d.host, port: d.port })} className="w-full text-left px-3 py-2 hover:bg-gray-800 flex justify-between">
                  <span className="mono">{d.host}:{d.port}</span>
                  <span className="text-gray-400">{[d.model || d.title, d.mode].filter(Boolean).join(' · ')}{d.likely ? ' ✓' : ''}</span>
                </button>
              ))}
            </div>
          )}
          {test && (
            <div className={`text-xs rounded-md border p-2 ${test.ok ? 'border-emerald-800 bg-emerald-950 text-emerald-200' : 'border-amber-800 bg-amber-950 text-amber-200'}`}>
              {test.ok
                ? <>Reading <b>{fmtWeight(test.latest?.weight, test.latest?.unit || s.unit)}</b>{test.latest?.motion ? ' (in motion)' : ''} · {test.frames} readings in {(test.durationMs / 1000).toFixed(1)} s ({test.framesPerSecond}/s)</>
                : <>✗ {test.error || 'Nothing readable arrived'}</>}
              {test.sample && <div className="mt-1 mono text-[10px] text-gray-400 break-all">{test.sample}</div>}
            </div>
          )}
          <label className="block text-xs text-gray-400">Weight unit
            <select className={inputClass} value={s.unit} onChange={(e) => patch('scale', { unit: e.target.value })}>
              <option value="lb">Pounds (lb)</option><option value="kg">Kilograms (kg)</option>
            </select>
          </label>
        </div>
        <div className="space-y-3">
          <h3 className="text-sm font-semibold">Detection weights</h3>
          {num('minWeight', 'A vehicle is on the deck at or above', 'Below this is a person, a pallet or an empty deck.')}
          {num('clearWeight', 'The deck is clear again under', 'The next photo waits for the weight to drop under this.')}
          <div className="grid grid-cols-2 gap-3">
            {num('stableSeconds', 'Must hold still for', 'A truck rocks for a moment after it stops.', { min: 0.5, step: 0.5, suffix: 'seconds' })}
            {num('stableTolerance', 'Allowing a wobble of', 'How much the reading may move and still count as settled.')}
          </div>
        </div>
      </section>
      <section className="border-t border-gray-800 pt-4 grid grid-cols-1 md:grid-cols-3 gap-5">
        <div className="space-y-2 text-xs text-gray-300">
          <span className="block font-medium text-gray-200">When a weight settles</span>
          <label className="flex items-center gap-2"><input type="checkbox" checked={s.capturePhoto !== false} onChange={(e) => patch('scale', { capturePhoto: e.target.checked })} /> Save a photo to the history</label>
          <label className="flex items-center gap-2"><input type="checkbox" checked={s.readPlate !== false} disabled={s.capturePhoto === false} onChange={(e) => patch('scale', { readPlate: e.target.checked })} /> Read the plate</label>
          <label className="flex items-center gap-2"><input type="checkbox" checked={s.stampPhoto !== false} disabled={s.capturePhoto === false} onChange={(e) => patch('scale', { stampPhoto: e.target.checked })} /> Stamp the weight on the photo</label>
        </div>
        <div className="space-y-1.5 text-xs text-gray-300">
          <span className="block font-medium text-gray-200">Show the live weight</span>
          {[['picture', 'Over the picture'], ['header', 'In the header bar'], ['section', 'As a readout strip'], ['none', 'Not on screen']].map(([key, label]) => (
            <label key={key} className="flex items-center gap-2"><input type="radio" name="display" checked={(s.display || 'picture') === key} onChange={() => patch('scale', { display: key })} /> {label}</label>
          ))}
        </div>
        <div className="text-xs text-gray-400">Corner of the picture for the weight
          <div className="grid grid-cols-2 gap-1.5 mt-1 max-w-xs">
            {CORNERS.map((c) => (
              <button key={c.key} type="button" onClick={() => patch('scale', { overlayCorner: c.key })} className={`px-2 py-1.5 text-xs rounded border ${s.overlayCorner === c.key ? 'bg-gray-100 text-black border-gray-100' : 'bg-gray-800 text-gray-300 border-gray-600 hover:bg-gray-700'}`}>{c.label}</button>
            ))}
          </div>
          <span className="block text-[11px] text-gray-500 mt-1">Used over the picture and on the photo. Pick the plate's corner and the two stack.</span>
        </div>
      </section>
      <section className="border-t border-gray-800 pt-4">
        <h3 className="text-sm font-semibold mb-2">Status</h3>
        {!live && <p className="text-xs text-gray-500">Not reading. Tick "Read the scale", enter the address and save.</p>}
        {live && (
          <div className="grid grid-cols-2 md:grid-cols-4 gap-x-4 gap-y-1 text-xs text-gray-300">
            <div><span className="text-gray-500">Connection</span> {live.connected ? <span className="text-emerald-300">connected</span> : <span className="text-amber-300">not connected</span>}</div>
            <div><span className="text-gray-500">Weight now</span> <b>{fmtWeight(live.weight, live.unit)}</b>{live.weight !== null && (live.motion ? ' · moving' : live.settled ? ' · settled' : ' · settling')}</div>
            <div><span className="text-gray-500">Readings</span> {live.frames}</div>
            <div><span className="text-gray-500">Last reading</span> {timeAgo(live.lastFrameAt)}</div>
            <div className="col-span-2"><span className="text-gray-500">Last weighing</span> {live.lastWeighing ? `${fmtWeight(live.lastWeighing.weight, live.lastWeighing.unit)} ${timeAgo(live.lastWeighing.at)}${live.lastWeighing.plate?.text ? ` · ${live.lastWeighing.plate.text}` : ''}` : 'none yet'}</div>
            {live.lastError && <div className="col-span-2 text-amber-300"><span className="text-gray-500">Last problem</span> {live.lastError} ({timeAgo(live.lastErrorAt)})</div>}
          </div>
        )}
      </section>
    </div>
  );
}
