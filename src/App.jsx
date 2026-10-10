import React, { useEffect, useState, useCallback } from 'react';
import CameraView from './components/CameraView';
import SettingsModal from './components/SettingsModal';
import HistoryDrawer from './components/HistoryDrawer';
import { fmtWeight } from './lib';

// The whole window: the camera with its overlays, a thin header, and two
// panels that slide over it - settings and history. Mirrors the Scale card
// on the Falcon site, grown to fill a screen.
export default function App() {
  const [info, setInfo] = useState(null);
  const [settings, setSettings] = useState(null);
  const [scale, setScale] = useState(null);
  const [showSettings, setShowSettings] = useState(false);
  const [showHistory, setShowHistory] = useState(false);
  const [lastEntry, setLastEntry] = useState(null);
  const [fullscreen, setFullscreen] = useState(false);
  const [updateReady, setUpdateReady] = useState('');

  const reload = useCallback(async () => {
    const [i, s, st] = await Promise.all([window.station.info(), window.station.settings.get(), window.station.scale.status()]);
    setInfo(i); setSettings(s); setScale(st);
  }, []);

  useEffect(() => {
    reload();
    const offs = [
      window.station.scale.onStatus(setScale),
      window.station.history.onEntry((entry) => setLastEntry(entry)),
      window.station.window.onFullscreen(setFullscreen),
      window.station.updates.onReady(setUpdateReady),
      window.station.scale.onPlate((plate) => setScale((s) => (s?.lastWeighing ? { ...s, lastWeighing: { ...s.lastWeighing, plate } } : s)))
    ];
    window.station.window.isFullscreen().then(setFullscreen);
    return () => offs.forEach((off) => off());
  }, [reload]);

  // A fresh install opens on the settings, since there is nothing to show yet.
  useEffect(() => {
    if (settings && !settings.camera.streamUrl && !settings.scale.host) setShowSettings(true);
  }, [settings]);

  useEffect(() => {
    const onKey = (e) => {
      if (e.key === 'F11') { e.preventDefault(); window.station.window.toggleFullscreen(); }
      if (e.key === 'Escape') { setShowSettings(false); setShowHistory(false); }
      if (e.key.toLowerCase() === 's' && !showSettings && !e.ctrlKey) setShowSettings(true);
      if (e.key.toLowerCase() === 'h' && !showSettings && !e.ctrlKey) setShowHistory((v) => !v);
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [showSettings]);

  if (!settings || !info) return <div className="h-full flex items-center justify-center text-gray-500">Starting…</div>;

  const display = settings.scale.enabled ? (settings.scale.display === 'picture' && !settings.camera.streamUrl ? 'section' : settings.scale.display) : 'none';
  const weightKnown = scale && scale.connected && scale.weight !== null && scale.weight !== undefined;
  const weightState = !scale ? '' : !scale.connected ? 'scale offline' : scale.settled ? 'settled' : scale.motion ? 'moving' : 'settling';

  return (
    <div className="h-full flex flex-col bg-black">
      {!fullscreen && (
        <div className="flex items-center justify-between gap-3 px-4 py-2 bg-gray-900 border-b border-gray-800 shrink-0">
          <div className="flex items-center gap-3 min-w-0">
            <span className="font-semibold text-gray-100 truncate">{settings.camera.name || 'Scale'}</span>
            {display === 'header' && scale && (
              <span className={`text-sm px-2 py-0.5 rounded border mono font-bold ${!scale.connected ? 'bg-gray-800 text-gray-400 border-gray-700' : scale.settled ? 'bg-emerald-900 text-emerald-200 border-emerald-600' : 'bg-gray-800 text-gray-200 border-gray-600'}`} title={`Truck scale: ${weightState}`}>
                ⚖ {weightKnown ? fmtWeight(scale.weight, scale.unit) : 'offline'}
              </span>
            )}
            {settings.scale.enabled && scale && !scale.connected && display !== 'header' && (
              <span className="text-xs text-red-400">scale offline</span>
            )}
          </div>
          <div className="flex items-center gap-2 shrink-0">
            {updateReady && (
              <button type="button" onClick={() => window.station.updates.install()} className="px-3 py-1 text-sm rounded bg-emerald-700 hover:bg-emerald-600 text-white" title="Closes the app, installs the update and opens it again">
                Restart to update to {updateReady}
              </button>
            )}
            <button type="button" onClick={() => setShowHistory((v) => !v)} className="px-3 py-1 text-sm rounded bg-gray-800 hover:bg-gray-700" title="History (H)">History</button>
            <button type="button" onClick={() => setShowSettings(true)} className="px-3 py-1 text-sm rounded bg-gray-800 hover:bg-gray-700" title="Settings (S)">Settings</button>
            <button type="button" onClick={() => window.station.window.toggleFullscreen()} className="px-3 py-1 text-sm rounded bg-gray-800 hover:bg-gray-700" title="Full screen (F11)">⛶</button>
          </div>
        </div>
      )}

      <div className="flex-1 min-h-0 relative">
        {settings.camera.streamUrl ? (
          <CameraView settings={settings} info={info} scale={scale} display={display} lastEntry={lastEntry} fullscreen={fullscreen} />
        ) : (
          <div className="absolute inset-0 flex flex-col items-center justify-center gap-3 text-gray-500">
            <p className="text-lg">No camera set up yet</p>
            <button type="button" onClick={() => setShowSettings(true)} className="px-4 py-2 rounded bg-emerald-700 text-white hover:bg-emerald-600">Open settings</button>
          </div>
        )}
        {display === 'section' && (
          <div className={`absolute left-0 right-0 bottom-0 px-6 py-4 bg-black/80 border-t border-gray-800 flex items-end justify-between gap-4 ${!settings.camera.streamUrl ? 'top-0 flex-col items-center justify-center' : ''}`}>
            <div>
              <div className="text-xs uppercase tracking-wide text-gray-400">Scale</div>
              <div className={`mono font-extrabold leading-none mt-1 ${scale?.settled ? 'text-emerald-400' : 'text-gray-100'} ${!settings.camera.streamUrl ? 'text-8xl' : 'text-5xl'}`}>
                {weightKnown ? fmtWeight(scale.weight, scale.unit) : '— —'}
              </div>
            </div>
            <div className="text-right text-sm text-gray-400">
              <div className={weightState === 'settled' ? 'text-emerald-400' : weightState === 'scale offline' ? 'text-red-400' : ''}>{weightState || 'waiting'}</div>
              {scale?.loaded && scale.lastWeighing?.plate?.text && <div className="mono text-2xl font-bold text-white mt-1">{scale.lastWeighing.plate.text}</div>}
              {scale?.lastWeighing && <div className="mt-1">Last weighing {fmtWeight(scale.lastWeighing.weight, scale.lastWeighing.unit)}</div>}
            </div>
          </div>
        )}
        {fullscreen && (
          <div className="absolute top-2 right-2 flex gap-1 opacity-30 hover:opacity-100 transition-opacity">
            <button type="button" onClick={() => setShowHistory((v) => !v)} className="px-2 py-1 text-xs rounded bg-black/70 text-gray-200">History</button>
            <button type="button" onClick={() => setShowSettings(true)} className="px-2 py-1 text-xs rounded bg-black/70 text-gray-200">Settings</button>
            <button type="button" onClick={() => window.station.window.toggleFullscreen()} className="px-2 py-1 text-xs rounded bg-black/70 text-gray-200">Exit full screen</button>
          </div>
        )}
        {showHistory && <HistoryDrawer info={info} onClose={() => setShowHistory(false)} />}
      </div>

      {showSettings && (
        <SettingsModal
          settings={settings}
          info={info}
          onSaved={(next) => { setSettings(next); reload(); }}
          onClose={() => setShowSettings(false)}
        />
      )}
    </div>
  );
}
