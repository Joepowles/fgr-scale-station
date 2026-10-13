import React, { useEffect, useRef, useState } from 'react';
import { fmtWeight, cornerClass } from '../lib';

// Seconds the picture may trail the newest frame received: past the first it
// plays fast until back under the last; past the middle one it reconnects.
const CATCH_UP_LAG_S = 1;
const MAX_LAG_S = 8;
const ON_TIME_LAG_S = 0.4;
// Zoom: how far in, how much one wheel notch does, and past what zoom the
// full-resolution stream is shown instead of the small one (the small one is
// 704x480 at the yard: blocks, zoomed). The switch waits for the zooming to
// stop, as each one reconnects.
const MAX_ZOOM = 10;
const WHEEL_STEP = 1.2;
const HD_FROM_ZOOM = 1.5;
const HD_SWITCH_DELAY_MS = 800;
const MIN_BOX_PX = 12;
const WHOLE = { s: 1, x: 0, y: 0 };

// Keeps a zoomed picture covering the view: no gap at an edge.
const clampView = ({ s, x, y }, w, h) => {
  const z = Math.min(MAX_ZOOM, Math.max(1, s));
  return { s: z, x: Math.min(0, Math.max(w - w * z, x)), y: Math.min(0, Math.max(h - h * z, y)) };
};

// The view showing box (in the picture's own coordinates at zoom 1), widened
// or made taller to the view's shape so nothing is stretched.
export const viewForBox = (box, w, h) => {
  let { x1, y1, x2, y2 } = box;
  let bw = Math.abs(x2 - x1);
  let bh = Math.abs(y2 - y1);
  const cx = (x1 + x2) / 2;
  const cy = (y1 + y2) / 2;
  if (bw / bh < w / h) bw = bh * (w / h); else bh = bw / (w / h);
  x1 = cx - bw / 2;
  y1 = cy - bh / 2;
  const s = w / bw;
  return clampView({ s, x: -x1 * s, y: -y1 * s }, w, h);
};

// The picture, and the chips over it: the plate in its corner, the weight in
// its own, stacked when they share one. A fresh <video> on every reconnect,
// because a stalled MediaSource never recovers on its own.
export default function CameraView({ settings, info, scale, display }) {
  const [tick, setTick] = useState(0);
  const [loaded, setLoaded] = useState(false);
  const [error, setError] = useState('');
  const [hd, setHd] = useState(false);
  // Zoom (issue #10): wheel to zoom about the pointer, drag a box to zoom
  // into it, right-click for the whole picture. Only the picture zooms; the
  // weight, plate and buttons stay where they are.
  const [view, setView] = useState(WHOLE);
  const [drag, setDrag] = useState(null); // { x1, y1, x2, y2 } in the view's pixels
  const [zoomHd, setZoomHd] = useState(false);
  const boxRef = useRef(null);
  const videoRef = useRef(null);
  const retry = useRef(null);
  const hasHd = !!settings.camera.hdStreamUrl && settings.camera.hdStreamUrl !== settings.camera.streamUrl;
  const showHd = hd || (hasHd && zoomHd);
  const src = `http://127.0.0.1:${info.serverPort}/stream?hd=${showHd ? 1 : 0}&t=${tick}`;

  useEffect(() => {
    const t = setTimeout(() => setZoomHd(view.s >= HD_FROM_ZOOM), HD_SWITCH_DELAY_MS);
    return () => clearTimeout(t);
  }, [view.s]);

  const size = () => {
    const r = boxRef.current.getBoundingClientRect();
    return { r, w: r.width, h: r.height };
  };
  const onWheel = (e) => {
    const { r, w, h } = size();
    const px = e.clientX - r.left;
    const py = e.clientY - r.top;
    setView((v) => {
      const s = Math.min(MAX_ZOOM, Math.max(1, v.s * (e.deltaY < 0 ? WHEEL_STEP : 1 / WHEEL_STEP)));
      // The point under the pointer stays under it.
      const cx = (px - v.x) / v.s;
      const cy = (py - v.y) / v.s;
      return clampView({ s, x: px - cx * s, y: py - cy * s }, w, h);
    });
  };
  const onMouseDown = (e) => {
    if (e.button !== 0 || e.target.closest('button')) return;
    const { r } = size();
    const x = e.clientX - r.left;
    const y = e.clientY - r.top;
    setDrag({ x1: x, y1: y, x2: x, y2: y });
  };
  const onMouseMove = (e) => {
    if (!drag) return;
    const { r } = size();
    setDrag((d) => d && { ...d, x2: e.clientX - r.left, y2: e.clientY - r.top });
  };
  const onMouseUp = () => {
    if (!drag) return;
    const d = drag;
    setDrag(null);
    if (Math.abs(d.x2 - d.x1) < MIN_BOX_PX || Math.abs(d.y2 - d.y1) < MIN_BOX_PX) return;
    const { w, h } = size();
    setView((v) => viewForBox({
      x1: (d.x1 - v.x) / v.s, y1: (d.y1 - v.y) / v.s, x2: (d.x2 - v.x) / v.s, y2: (d.y2 - v.y) / v.s
    }, w, h));
  };
  const onContextMenu = (e) => { e.preventDefault(); setDrag(null); setView(WHOLE); };

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
    // The player shows every frame it is handed at normal speed, so each
    // stall (a busy CPU, a retransmit) adds delay that is never made up and
    // the picture falls further behind the yard. Play a little fast while
    // behind; past saving, reconnect, which starts at the newest keyframe.
    // Taken out in beta.7 in the belief it made the picture gray; that was
    // the camera's undetected frame rate (issue #8).
    const catchUp = setInterval(() => {
      if (!v.buffered.length || v.paused) return;
      const lag = v.buffered.end(v.buffered.length - 1) - v.currentTime;
      if (lag > MAX_LAG_S) setTick((t) => t + 1);
      else if (lag > CATCH_UP_LAG_S) v.playbackRate = 1.5;
      else if (lag < ON_TIME_LAG_S) v.playbackRate = 1;
    }, 500);
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
      clearInterval(catchUp);
    };
  }, [src, info.ffmpeg]);

  // What the player is showing, into the station log once a minute, so a bug
  // report says whether a bad picture is the stream or the player.
  useEffect(() => {
    const t = setInterval(() => {
      const v = videoRef.current;
      if (!v || !v.buffered.length) return;
      const q = v.getVideoPlaybackQuality?.() || {};
      const behind = v.buffered.end(v.buffered.length - 1) - v.currentTime;
      window.station.log.write(`[view] ${v.videoWidth}x${v.videoHeight}, ${behind.toFixed(1)}s buffered ahead at ${v.playbackRate}x, ${q.droppedVideoFrames ?? '?'} of ${q.totalVideoFrames ?? '?'} frames dropped, ${v.paused ? 'paused' : 'playing'}`);
    }, 60 * 1000);
    return () => clearInterval(t);
  }, []);

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
    <div ref={boxRef} className={`absolute inset-0 bg-black overflow-hidden ${drag ? 'cursor-crosshair' : view.s > 1 ? 'cursor-zoom-out' : 'cursor-zoom-in'}`}
      onWheel={onWheel} onMouseDown={onMouseDown} onMouseMove={onMouseMove} onMouseUp={onMouseUp} onMouseLeave={onMouseUp} onContextMenu={onContextMenu}>
      <video key={src} ref={videoRef} src={src} autoPlay muted playsInline className="absolute inset-0 w-full h-full object-contain origin-top-left"
        style={{ transform: `translate(${view.x}px, ${view.y}px) scale(${view.s})` }} />
      {drag && (
        <div className="absolute border-2 border-dashed border-white bg-white/10 pointer-events-none"
          style={{ left: Math.min(drag.x1, drag.x2), top: Math.min(drag.y1, drag.y2), width: Math.abs(drag.x2 - drag.x1), height: Math.abs(drag.y2 - drag.y1) }} />
      )}
      {view.s > 1 && (
        <div className="absolute top-10 right-3 px-2 py-0.5 text-[11px] rounded bg-black/60 text-gray-200 pointer-events-none">
          {view.s.toFixed(1)}×{showHd ? ' · HD' : ''} · right-click for the whole picture
        </div>
      )}
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
      {/* A picture that comes up gray and blocky can be cured by connecting
          to the camera afresh; this does that without restarting the app. */}
      <button type="button" onClick={async () => { await window.station.camera.reconnect(); setTick((t) => t + 1); }} className="absolute top-3 right-3 px-2 py-0.5 text-[11px] rounded border opacity-40 hover:opacity-100 bg-black/60 text-gray-200 border-gray-500" title="Connect to the camera again, for a picture that is gray or frozen">Reconnect</button>
      {hasHd && (
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
