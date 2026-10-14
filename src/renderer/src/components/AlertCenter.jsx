import React, { useEffect, useRef, useState } from 'react';
import { playSound } from '../alertSounds';

const BANNER_SECONDS = 8;

// Where alerts are heard and seen in the main window: each one's sound, and
// a banner across the top for the ones that want it. An alert that plays
// until dismissed always has a banner, with the button to stop it. While the
// alerts are muted, a small chip says until when, with Unmute.
export default function AlertCenter() {
  const [alerts, setAlerts] = useState([]);
  const [mutedUntil, setMutedUntil] = useState(null);
  const stops = useRef(new Map());
  const next = useRef(0);

  const dismiss = (key) => {
    stops.current.get(key)?.();
    stops.current.delete(key);
    setAlerts((list) => list.filter((a) => a.key !== key));
  };

  useEffect(() => {
    if (!window.station.alerts) return undefined;
    window.station.alerts.status().then((s) => setMutedUntil(s?.mutedUntil || null)).catch(() => {});
    const offStatus = window.station.alerts.onStatus((s) => setMutedUntil(s?.mutedUntil || null));
    const offAlert = window.station.alerts.onAlert(async (a) => {
      const key = next.current++;
      const forever = a.length === 'dismiss';
      const soundUrl = a.sound === 'custom' && a.soundFile ? await window.station.alerts.soundUrl(a.soundFile) : null;
      const stop = playSound({ sound: a.sound, soundUrl, volume: a.volume, once: a.length === 'once', seconds: a.lengthSeconds, forever });
      stops.current.set(key, stop);
      if (a.banner || forever) {
        setAlerts((list) => [...list.slice(-4), { ...a, key, forever }]);
        if (!forever) setTimeout(() => dismiss(key), Math.max(BANNER_SECONDS, a.length === 'seconds' ? a.lengthSeconds : 0) * 1000);
      } else if (!forever) {
        setTimeout(() => { stops.current.get(key)?.(); stops.current.delete(key); }, (a.length === 'seconds' ? a.lengthSeconds : 30) * 1000);
      }
    });
    return () => { offStatus(); offAlert(); for (const stop of stops.current.values()) stop(); };
  }, []);

  const muted = mutedUntil && mutedUntil > Date.now();
  if (!alerts.length && !muted) return null;
  return (
    <div className="fixed top-14 left-1/2 -translate-x-1/2 z-[60] flex flex-col items-center gap-2 w-full max-w-xl px-3 pointer-events-none">
      {alerts.map((a) => (
        <div key={a.key} className={`pointer-events-auto w-full rounded-lg border shadow-2xl px-4 py-2.5 flex items-center gap-3 ${a.forever ? 'bg-red-950/95 border-red-400' : 'bg-gray-900/95 border-amber-400'}`}>
          <span className="text-2xl" aria-hidden>{a.forever ? '🚨' : '🔔'}</span>
          <div className="flex-1 min-w-0">
            <div className="font-semibold text-white">{a.name || 'Alert'}</div>
            <div className="text-sm text-gray-200">{a.message}{a.silenced ? <span className="text-gray-400"> · silent ({a.silenced})</span> : ''}</div>
          </div>
          <button type="button" onClick={() => dismiss(a.key)} className={`px-3 py-1.5 text-sm rounded-md ${a.forever ? 'bg-red-600 hover:bg-red-500' : 'bg-gray-700 hover:bg-gray-600'} text-white`}>Dismiss</button>
        </div>
      ))}
      {muted && (
        <div className="pointer-events-auto px-3 py-1 rounded-full bg-black/70 border border-gray-600 text-xs text-gray-200 flex items-center gap-2">
          🔕 Alerts muted until {new Date(mutedUntil).toLocaleTimeString([], { hour: 'numeric', minute: '2-digit' })}
          <button type="button" onClick={() => window.station.alerts.mute(0)} className="underline hover:text-white">Unmute</button>
        </div>
      )}
    </div>
  );
}
