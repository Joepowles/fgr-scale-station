const { spawn } = require('child_process');

/**
 * RTSP -> fragmented MP4 for the window's <video> element. Ported from the
 * Falcon backend's cameraStreamService; the station is on the camera's own
 * LAN, so bandwidth matters less than smoothness here.
 *
 * Measured against the Front camera (hevc 960x480), 12s samples:
 *
 *   MJPEG as shipped (q15, 5fps)   0.84 Mbit/s   0.7 CPU-s   ~4.5 fps
 *   H.264 fMP4, full framerate     0.61 Mbit/s   3.7 CPU-s
 *   H.264 fMP4, capped to 5fps     0.48 Mbit/s   0.8 CPU-s
 *
 * So this is a real but moderate win, not a dramatic one: at full framerate it
 * uses less bandwidth than the 5fps MJPEG it replaces while being roughly four
 * times smoother, and H.264 in a <video> element is far cheaper for the browser
 * than decoding a whole JPEG per frame into an <img>. The cost is encoder CPU,
 * about a third of a core per camera while someone is actually watching. Set
 * CAMERA_STREAM_FPS to cap the framerate on a box where that matters; at 5fps
 * the CPU cost matches MJPEG's and the bandwidth is still lower.
 *
 * Transcoding rather than remuxing is forced by the hardware: all three cameras
 * emit HEVC, and browser HEVC support through Media Source Extensions is patchy
 * (Firefox has none). Copying the stream untouched would be nearly free - 0.1
 * CPU-seconds - but only some viewers could play it. If these are ever
 * reconfigured to emit H.264 substreams, this should switch to '-c copy' and
 * the encode cost disappears entirely.
 */

const { ffmpegPath } = require('./ffmpegPath');

// One encoder per camera, shared by every viewer - the same arrangement the
// MJPEG path uses, and for the same reason: cost should scale with cameras,
// not with cameras x viewers. Keyed by the full RTSP URL including
// credentials, so two cards on one channel share and different logins do not.
const streams = new Map();

const MAX_RESTARTS = 50;
const RESTART_DELAY_MS = 2000;
const LINGER_MS = 5000;
const IDLE_INIT_TIMEOUT_MS = 15000;
const MAX_ENCODER_LAG_S = 5;
const WARN_EVERY_MS = 60 * 1000;
const FULL_LINES_PER_CONNECTION = 40;

// ffmpeg can repeat a complaint many times a second; the log gets it once a
// minute, with how many were held back.
const warnAtMostEvery = (state, key, message) => {
  const w = (state.warned ??= {})[key] ??= { at: 0, held: 0 };
  if (Date.now() - w.at < WARN_EVERY_MS) { w.held += 1; return; }
  console.warn(`[camera-stream] ${message}${w.held ? ` (and ${w.held} more like it)` : ''}`);
  w.at = Date.now();
  w.held = 0;
};

/**
 * How many seconds ffmpeg has fallen behind since its first progress line.
 *
 * Its stats carry time=, the media time written so far. Live, that should
 * advance with the clock; wall time minus media time is the delay, and its
 * growth beyond the first reading (connecting, the first keyframe) is lag.
 */
const trackLag = (state, text, now) => {
  const m = /time=\s*(\d+):(\d+):([\d.]+)/.exec(text);
  if (!m) return 0;
  const offset = now / 1000 - (Number(m[1]) * 3600 + Number(m[2]) * 60 + Number(m[3]));
  if (state.lagBaseline == null) state.lagBaseline = offset;
  return offset - state.lagBaseline;
};

/**
 * Split a byte stream into whole MP4 boxes.
 *
 * fMP4 is not self-synchronising the way MJPEG is: a viewer who joins mid
 * stream and simply starts receiving bytes gets garbage. Every viewer needs
 * the initialisation segment (ftyp + moov) first, then fragments starting at a
 * moof boundary. So the stream is parsed into top-level boxes: everything up
 * to the first moof is kept as the init segment and replayed to each new
 * viewer, and whole boxes after it are broadcast live. A moof is held back
 * until its mdat has arrived and the two go out as one write: a viewer who
 * joined between them would otherwise get media data with no header for it,
 * and the player stalls.
 */
const consumeBoxes = (state, chunk) => {
  state.buffer = state.buffer.length ? Buffer.concat([state.buffer, chunk]) : chunk;

  for (;;) {
    if (state.buffer.length < 8) return;
    const size = state.buffer.readUInt32BE(0);
    const type = state.buffer.toString('ascii', 4, 8);

    // Size 0 means "to end of file" and size 1 means a 64-bit length follows.
    // Neither is produced by the fragmenting muxer here; treating them as
    // unparseable is safer than guessing and desynchronising the stream.
    if (size < 8) {
      console.warn(`[camera-stream] unparseable box '${type}' (size ${size}); restarting`);
      restart(state);
      return;
    }
    if (state.buffer.length < size) return;

    const box = state.buffer.subarray(0, size);
    state.buffer = state.buffer.subarray(size);

    if (!state.initReady) {
      state.initParts.push(box);
      // moov completes the init segment; moof means the muxer has moved on.
      if (type === 'moov') {
        state.initSegment = Buffer.concat(state.initParts);
        state.initParts = [];
        state.initReady = true;
        flushPending(state);
      }
      continue;
    }

    if (type === 'moof') {
      state.heldMoof = box;
    } else if (type === 'mdat' && state.heldMoof) {
      broadcast(state, Buffer.concat([state.heldMoof, box]));
      state.heldMoof = null;
    } else {
      broadcast(state, box);
    }
  }
};

const writeTo = (res, buf) => {
  if (res.writableEnded || res.destroyed) return false;
  try { res.write(buf); return true; } catch { return false; }
};

// Viewers that arrived before the init segment existed. They cannot be sent
// anything until it does, so they wait rather than receive unplayable bytes.
const flushPending = (state) => {
  for (const res of state.pending) {
    if (writeTo(res, state.initSegment)) state.subscribers.add(res);
  }
  state.pending.clear();
};

const broadcast = (state, box) => {
  for (const res of state.subscribers) {
    if (!writeTo(res, box)) {
      state.subscribers.delete(res);
      try { if (!res.writableEnded) res.end(); } catch {}
    }
  }
  if (!state.subscribers.size && !state.pending.size) scheduleLinger(state);
};

const scheduleLinger = (state) => {
  if (state.lingerTimer) return;
  // Kept alive briefly after the last viewer leaves so a page refresh does not
  // pay the RTSP reconnect and a fresh init segment.
  state.lingerTimer = setTimeout(() => {
    state.lingerTimer = null;
    if (!state.subscribers.size && !state.pending.size) stop(state);
  }, LINGER_MS);
  state.lingerTimer.unref?.();
};

const startFfmpeg = (state) => {
  if (!state.active) return;

  const args = [
    '-rtsp_transport', 'tcp',
    '-fflags', 'nobuffer',
    '-i', state.url,
    '-an',
    '-c:v', 'libx264',
    '-preset', 'ultrafast',
    '-tune', 'zerolatency',
    '-b:v', String(process.env.CAMERA_STREAM_BITRATE || '1500k'),
    // Forces a keyframe about every two seconds. Fragments are cut at
    // keyframes, so this is also how long a new viewer waits for playable
    // video - the camera's own GOP can be several seconds.
    '-g', '30',
    ...(process.env.CAMERA_STREAM_FPS ? ['-r', String(process.env.CAMERA_STREAM_FPS)] : []),
    '-f', 'mp4',
    '-movflags', 'frag_keyframe+empty_moov+default_base_moof',
    '-'
  ];

  console.log('[camera-stream] connecting to the camera');
  const ff = spawn(ffmpegPath(), args, { stdio: ['ignore', 'pipe', 'pipe'], windowsHide: true });
  state.ff = ff;

  ff.stdout.on('data', (chunk) => consumeBoxes(state, chunk));

  ff.stderr.on('data', (d) => {
    const text = String(d);
    // Everything ffmpeg says about the connection goes in the log: the
    // first lines of each connection in full (what the camera sends, any
    // trouble starting), then at most one line a minute. The picture
    // sometimes comes up gray and blocky (issues #6, #7), and complaints such
    // as "RTP: missed packets" were being left out. ffmpeg quotes the input
    // URL back on failure, so credentials are taken out first.
    for (const raw of text.split(/\r?\n|\r/)) {
      const line = raw.trim().replace(/rtsp:\/\/[^@\s]*@/g, 'rtsp://***@');
      if (!line || /^frame=/.test(line)) continue;
      state.linesLogged = (state.linesLogged || 0) + 1;
      if (state.linesLogged <= FULL_LINES_PER_CONNECTION) console.log(`[camera-stream] ${line}`);
      else warnAtMostEvery(state, 'ffmpeg', line);
    }
    const progress = /frame=.*speed=\s*\S+/.exec(text);
    if (progress) warnAtMostEvery(state, 'progress', progress[0].replace(/\s+/g, ' '));
    // Falling behind the camera, from a busy CPU or a camera clock that runs
    // slow, is only reported: beta.4 restarted ffmpeg for it.
    const behind = trackLag(state, text, Date.now());
    if (behind > MAX_ENCODER_LAG_S) warnAtMostEvery(state, 'lag', `${behind.toFixed(1)}s behind the camera`);
  });

  ff.on('close', (code) => {
    if (!state.active || state.ff !== ff) return;
    state.restartCount += 1;
    if (state.restartCount > MAX_RESTARTS) {
      console.error(`[camera-stream] giving up after ${MAX_RESTARTS} restarts`);
      stop(state);
      return;
    }
    console.log(`[camera-stream] ffmpeg exited (code ${code}); restart ${state.restartCount}/${MAX_RESTARTS} in ${RESTART_DELAY_MS}ms (${state.subscribers.size + state.pending.size} viewer(s))`);
    restart(state);
  });
};

const restart = (state) => {
  try { if (state.ff) state.ff.kill('SIGKILL'); } catch {}
  state.ff = null;
  // A restart produces a new init segment, and viewers holding the old one
  // cannot decode fragments belonging to the new one. They are dropped so the
  // player reconnects cleanly rather than stalling on undecodable data.
  for (const res of [...state.subscribers, ...state.pending]) {
    try { if (!res.writableEnded) res.end(); } catch {}
  }
  state.subscribers.clear();
  state.pending.clear();
  state.buffer = Buffer.alloc(0);
  state.initParts = [];
  state.initSegment = null;
  state.initReady = false;
  state.heldMoof = null;
  state.lagBaseline = null;
  state.warned = {};
  state.linesLogged = 0;
  if (state.restartTimer) clearTimeout(state.restartTimer);
  state.restartTimer = setTimeout(() => { state.restartTimer = null; startFfmpeg(state); }, RESTART_DELAY_MS);
  state.restartTimer.unref?.();
};

const stop = (state) => {
  if (!state.active) return;
  state.active = false;
  if (state.restartTimer) { clearTimeout(state.restartTimer); state.restartTimer = null; }
  if (state.lingerTimer) { clearTimeout(state.lingerTimer); state.lingerTimer = null; }
  try { if (state.ff) state.ff.kill('SIGKILL'); } catch {}
  state.ff = null;
  streams.delete(state.url);
  for (const res of [...state.subscribers, ...state.pending]) {
    try { if (!res.writableEnded) res.end(); } catch {}
  }
  state.subscribers.clear();
  state.pending.clear();
};

/**
 * Attach an HTTP response to a camera as fragmented MP4.
 *
 * @param {string} rtspUrl full RTSP URL including credentials
 * @param {object} res     express response
 */
const streamRtspToFmp4 = (rtspUrl, res) => {
  res.writeHead(200, {
    'Content-Type': 'video/mp4',
    'Cache-Control': 'no-cache, no-store',
    'Connection': 'close',
    'X-Accel-Buffering': 'no'
  });

  let state = streams.get(rtspUrl);
  if (!state) {
    state = {
      url: rtspUrl, active: true, ff: null,
      subscribers: new Set(), pending: new Set(),
      buffer: Buffer.alloc(0), initParts: [], initSegment: null, initReady: false, heldMoof: null,
      restartCount: 0, restartTimer: null, lingerTimer: null
    };
    streams.set(rtspUrl, state);
    startFfmpeg(state);
  }

  if (state.lingerTimer) { clearTimeout(state.lingerTimer); state.lingerTimer = null; }

  if (state.initReady && state.initSegment) {
    if (writeTo(res, state.initSegment)) state.subscribers.add(res);
  } else {
    state.pending.add(res);
    // A camera that accepts the connection but never produces a decodable
    // stream would otherwise leave the viewer waiting on a blank player
    // forever with no indication anything is wrong.
    const giveUp = setTimeout(() => {
      if (state.pending.delete(res)) {
        try { if (!res.writableEnded) res.end(); } catch {}
      }
    }, IDLE_INIT_TIMEOUT_MS);
    giveUp.unref?.();
  }

  const detach = () => {
    state.subscribers.delete(res);
    state.pending.delete(res);
    if (!state.subscribers.size && !state.pending.size) scheduleLinger(state);
  };
  res.on('close', detach);
  res.on('error', detach);
};

// The Reconnect button: a fresh connection to the camera for every stream,
// for a picture that came up gray. Viewers are cut off and come back.
const reconnectCameraStreams = () => {
  for (const state of streams.values()) {
    if (!state.active) continue;
    console.log('[camera-stream] reconnecting, asked from the window');
    restart(state);
  }
};

const stopAllCameraStreams = () => {
  for (const state of [...streams.values()]) stop(state);
};

const activeCameraStreams = () =>
  [...streams.values()].map((s) => ({
    viewers: s.subscribers.size + s.pending.size,
    ready: s.initReady,
    restarts: s.restartCount
  }));

module.exports = { streamRtspToFmp4, stopAllCameraStreams, reconnectCameraStreams, activeCameraStreams, consumeBoxes, trackLag };
