import React, { useState } from 'react';
import { inputClass, btn } from '../lib';

// Find the camera on the LAN, pick its streams, check a frame comes through.
export default function CameraTab({ draft, patch, info }) {
  const c = draft.camera;
  const [finding, setFinding] = useState(false);
  const [found, setFound] = useState(null);
  const [streams, setStreams] = useState(null);
  const [listing, setListing] = useState(false);
  const [test, setTest] = useState(null);
  const [testing, setTesting] = useState(false);
  const [error, setError] = useState('');

  const discover = async () => {
    setFinding(true); setError(''); setFound(null);
    try { setFound(await window.station.camera.discover()); } catch (err) { setError(err.message); } finally { setFinding(false); }
  };
  const listStreams = async (host = c.host) => {
    setListing(true); setError(''); setStreams(null);
    try {
      const list = await window.station.camera.streams({ host, username: c.username, password: c.password });
      setStreams(list);
      if (!list.length) setError('The camera listed no streams. Check the login.');
    } catch (err) { setError(err.message); } finally { setListing(false); }
  };
  const pick = (uri, which) => {
    if (which === 'main') patch('camera', { hdStreamUrl: uri });
    else patch('camera', { streamUrl: uri });
  };
  const testFrame = async (url) => {
    setTesting(true); setTest(null); setError('');
    try { setTest(await window.station.camera.snapshotTest({ url, username: c.username, password: c.password })); } catch (err) { setError(err.message); } finally { setTesting(false); }
  };

  const bySize = (a, b) => (b.width || 0) * (b.height || 0) - (a.width || 0) * (a.height || 0);

  return (
    <div className="space-y-5">
      <section className="grid grid-cols-1 md:grid-cols-2 gap-5">
        <div className="space-y-3">
          <h3 className="text-sm font-semibold">Camera</h3>
          <label className="block text-xs text-gray-400">Name shown in the header
            <input className={inputClass} value={c.name} onChange={(e) => patch('camera', { name: e.target.value })} />
          </label>
          <div className="grid grid-cols-3 gap-2">
            <label className="block text-xs text-gray-400 col-span-3">Camera address
              <div className="flex gap-2 mt-1">
                <input className={inputClass} value={c.host} placeholder="192.168.1.229" onChange={(e) => patch('camera', { host: e.target.value })} />
                <button type="button" onClick={discover} disabled={finding} className={`${btn} whitespace-nowrap`}>{finding ? 'Searching…' : 'Find cameras'}</button>
              </div>
            </label>
            <label className="block text-xs text-gray-400">Username<input className={inputClass} value={c.username} onChange={(e) => patch('camera', { username: e.target.value })} /></label>
            <label className="block text-xs text-gray-400">Password<input type="password" className={inputClass} value={c.password} onChange={(e) => patch('camera', { password: e.target.value })} /></label>
            <label className="block text-xs text-gray-400">ONVIF port<input type="number" className={inputClass} value={c.onvifPort} onChange={(e) => patch('camera', { onvifPort: Number(e.target.value) })} /></label>
          </div>
          {found && (
            <div className="border border-gray-700 rounded-md divide-y divide-gray-800 text-sm">
              {!found.length && <p className="p-2 text-gray-500 text-xs">No cameras answered. They must be on this PC's network and have ONVIF turned on.</p>}
              {found.map((cam) => (
                <button key={cam.host} type="button" onClick={() => { patch('camera', { host: cam.host }); listStreams(cam.host); }} className="w-full text-left px-3 py-2 hover:bg-gray-800 flex justify-between">
                  <span className="mono">{cam.host}</span>
                  <span className="text-gray-400 text-xs">{[cam.name, cam.hardware].filter(Boolean).join(' · ')}</span>
                </button>
              ))}
            </div>
          )}
          <button type="button" onClick={() => listStreams()} disabled={listing || !c.host} className={btn}>{listing ? 'Asking the camera…' : 'List this camera\'s streams'}</button>
          {streams && (
            <div className="border border-gray-700 rounded-md divide-y divide-gray-800 text-xs">
              {streams.slice().sort(bySize).map((s) => (
                <div key={s.profileToken} className="px-3 py-2 flex items-center gap-2">
                  <div className="flex-1 min-w-0">
                    <div className="text-gray-200">{s.profileName || s.profileToken}{s.width ? ` · ${s.width}×${s.height}` : ''}</div>
                    <div className="mono text-gray-500 truncate">{s.streamUri}</div>
                  </div>
                  <button type="button" onClick={() => pick(s.streamUri, 'view')} className={btn}>Use for view</button>
                  <button type="button" onClick={() => pick(s.streamUri, 'main')} className={btn}>Use for plates</button>
                </div>
              ))}
            </div>
          )}
        </div>
        <div className="space-y-3">
          <h3 className="text-sm font-semibold">Streams</h3>
          <label className="block text-xs text-gray-400">Live view stream (the small one)
            <input className={`${inputClass} mono`} value={c.streamUrl} placeholder="rtsp://…/cam/realmonitor?channel=1&subtype=1" onChange={(e) => patch('camera', { streamUrl: e.target.value })} />
          </label>
          <label className="block text-xs text-gray-400">Full-resolution stream (plates and photos)
            <input className={`${inputClass} mono`} value={c.hdStreamUrl} placeholder="rtsp://…/cam/realmonitor?channel=1&subtype=0" onChange={(e) => patch('camera', { hdStreamUrl: e.target.value })} />
            <span className="text-[11px] text-gray-500">Plates are far too small on the live-view stream. Leave blank to use the live view stream for everything.</span>
          </label>
          <div className="flex gap-2">
            <button type="button" onClick={() => testFrame(c.streamUrl)} disabled={testing || !c.streamUrl} className={btn}>Test live view</button>
            <button type="button" onClick={() => testFrame(c.hdStreamUrl || c.streamUrl)} disabled={testing || !(c.hdStreamUrl || c.streamUrl)} className={btn}>Test full-resolution</button>
          </div>
          {testing && <p className="text-xs text-gray-400">Grabbing a frame…</p>}
          {test && (
            <div className="space-y-1">
              <p className="text-xs text-emerald-300">Got a {test.width}×{test.height} frame ({Math.round(test.bytes / 1024)} KB)</p>
              <img src={test.image} alt="" className="w-full rounded bg-black" />
            </div>
          )}
          <label className="flex items-center gap-2 text-xs text-gray-300">
            <input type="checkbox" checked={!!c.showClock} onChange={(e) => patch('camera', { showClock: e.target.checked })} /> Show the PC's clock over the picture
          </label>
          {!info.ffmpeg && <p className="text-xs text-amber-400">ffmpeg is missing from this build, so there is no live view or snapshots.</p>}
        </div>
      </section>
      {error && <p className="text-xs text-red-400">{error}</p>}
      <section className="grid grid-cols-1 md:grid-cols-3 gap-3 border-t border-gray-800 pt-4">
        <label className="flex items-center gap-2 text-xs text-gray-300"><input type="checkbox" checked={!!draft.window.fullscreen} onChange={(e) => patch('window', { fullscreen: e.target.checked })} /> Open full screen (F11 toggles)</label>
        <label className="flex items-center gap-2 text-xs text-gray-300"><input type="checkbox" checked={!!draft.window.alwaysOnTop} onChange={(e) => patch('window', { alwaysOnTop: e.target.checked })} /> Keep the window on top</label>
        <label className="flex items-center gap-2 text-xs text-gray-300"><input type="checkbox" checked={!!draft.window.startWithWindows} onChange={(e) => patch('window', { startWithWindows: e.target.checked })} /> Start with Windows</label>
      </section>
    </div>
  );
}
