import React, { useEffect, useRef, useState } from 'react';
import { fmtWeight, cornerClass } from '../lib';

// The picture, and the chips over it: the plate in its corner, the weight in
// its own, stacked when they share one. A fresh <video> on every reconnect,
// because a stalled MediaSource never recovers on its own.
export default function CameraView({ settings, info, scale, display }) {
  const [tick, setTick] = useState(0);
  const [loaded, setLoaded] = useState(false);
  const [error, setError] = useState('');
  const [hd, setHd] = useState(false);
  const videoRef = useRef(null);
  const retry = useRef(null);
  const src = `http://127.0.0.1:${info.serverPort}/stream?hd=${hd ? 1 : 0}&t=${tick}`;

  useEffect(() => {
    setLoaded(false);
    setError('');
    const v = videoRef.current;
    if (!v) return undefined;
    v.muted = true;
    v.play().catch(() => {});
    const onPlaying = () => { setLoaded(true); setError(''); };
    const onError = () => {
      setError(info.ffmpeg ? 'Camera not responding' : 'ffmpeg is missing from this build');
      clearTimeout(retry.current);
      retry.current = setTimeout(() => setTick((t) => t + 1), 5000);
    };
    const onStalled = () => { clearTimeout(retry.current); retry.current = setTimeout(() => setTick((t) => t + 1), 8000); };
    v.addEventListener('playing', onPlaying);
    v.addEventListener('error', onError);
    v.addEventListener('stalled', onStalled);
    v.addEventListener('ended', onError);
    return () => {
      v.removeEventListener('playing', onPlaying);
      v.removeEventListener('error', onError);
      v.removeEventListener('stalled', onStalled);
      v.removeEventListener('ended', onError);
      clearTimeout(retry.current);
    };
  }, [src, info.ffmpeg]);

  // A long session drifts; reconnect every half hour like the site's cards do.
  useEffect(() => {
    const t = setInterval(() => setTick((x) => x + 1), 30 * 60 * 1000);
    return () => clearInterval(t);
  }, []);

  const plateCorner = settings.plates.overlayCorner || 'bottom-left';
  const weightCorner = settings.scale.overlayCorner || 'bottom-right';
  const showWeight = display === 'picture' && scale && scale.connected && scale.weight !== null && scale.weight !== undefined;
  const plate = scale?.loaded && scale.lastWeighing?.plate?.text ? scale.lastWeighing.plate : null;
  const showPlate = settings.plates.enabled && !!plate;
  const share = showPlate && showWeight && plateCorner === weightCorner;

  const plateChip = showPlate ? (
    <div className="rounded-md bg-black/90 border-2 border-white text-white mono font-extrabold tracking-widest leading-none shadow-lg text-[clamp(20px,4vw,64px)] px-[0.5em] py-[0.25em]" title={`Plate read at ${Math.round((plate.confidence || 0) * 100)}% confidence`}>
      {plate.text}
    </div>
  ) : null;
  const weightChip = showWeight ? (
    <div className={`rounded-md border-2 mono font-extrabold tracking-wider leading-none shadow-lg text-[clamp(18px,3.4vw,56px)] px-[0.45em] py-[0.22em] ${scale.settled ? 'bg-emerald-900/90 border-emerald-200 text-white' : 'bg-black/70 border-gray-400 text-gray-200'}`} title={scale.settled ? 'Settled weight on the scale' : scale.motion ? 'Scale in motion' : 'Scale reading settling'}>
      {fmtWeight(scale.weight, scale.unit)}
    </div>
  ) : null;

  return (
    <div className="absolute inset-0 bg-black">
      <video key={src} ref={videoRef} src={src} autoPlay muted playsInline className="absolute inset-0 w-full h-full object-contain" />
      {!loaded && (
        <div className="absolute inset-0 flex flex-col items-center justify-center gap-2 pointer-events-none">
          <div className="w-8 h-8 border-2 border-gray-600 border-t-gray-200 rounded-full animate-spin" />
          <p className="text-gray-400 text-sm">{error || 'Connecting to camera…'}</p>
        </div>
      )}
      <div className="absolute inset-0 pointer-events-none">
        {plateChip && (
          <div className={`absolute flex flex-col gap-2 ${cornerClass(plateCorner)}`}>
            {plateChip}
            {share && weightChip}
          </div>
        )}
        {weightChip && !share && (
          <div className={`absolute flex flex-col gap-2 ${cornerClass(weightCorner)}`}>{weightChip}</div>
        )}
        {settings.camera.showClock && <Clock />}
      </div>
      {settings.camera.hdStreamUrl && settings.camera.hdStreamUrl !== settings.camera.streamUrl && (
        <button type="button" onClick={() => { setHd((v) => !v); setTick((t) => t + 1); }} className={`absolute bottom-3 left-1/2 -translate-x-1/2 px-2 py-0.5 text-[11px] rounded border opacity-40 hover:opacity-100 ${hd ? 'bg-white text-black border-white' : 'bg-black/60 text-gray-200 border-gray-500'}`} title="Switch between the small stream and the full-resolution one">HD</button>
      )}
    </div>
  );
}

function Clock() {
  const [now, setNow] = useState(new Date());
  useEffect(() => { const t = setInterval(() => setNow(new Date()), 1000); return () => clearInterval(t); }, []);
  return (
    <div className="absolute top-3 left-1/2 -translate-x-1/2 px-2 py-0.5 rounded bg-black/50 text-gray-200 text-xs mono">
      {now.toLocaleString([], { month: 'short', day: 'numeric', hour: 'numeric', minute: '2-digit', second: '2-digit' })}
    </div>
  );
}
