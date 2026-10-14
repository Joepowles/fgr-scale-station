// The alerts' built-in sounds, made in the window with Web Audio: nothing to
// ship, and every one at any volume. A custom sound is a file on the PC,
// played through the app's local server.

export const SOUNDS = [
  { key: 'chime', label: 'Chime' },
  { key: 'bell', label: 'Bell' },
  { key: 'beep', label: 'Beep' },
  { key: 'double', label: 'Double beep' },
  { key: 'alarm', label: 'Alarm' },
  { key: 'custom', label: 'A sound file…' },
  { key: 'none', label: 'No sound' }
];

let context = null;
const audio = () => {
  if (!context) context = new (window.AudioContext || window.webkitAudioContext)();
  if (context.state === 'suspended') context.resume().catch(() => {});
  return context;
};

// One tone: frequency (or [from, to] to sweep), when it starts, how long,
// its wave and volume, fading out at the end.
const tone = (ctx, out, { freq, start, length, wave = 'sine', gain = 1, decay = false }) => {
  const osc = ctx.createOscillator();
  const g = ctx.createGain();
  osc.type = wave;
  const t0 = ctx.currentTime + start;
  if (Array.isArray(freq)) {
    osc.frequency.setValueAtTime(freq[0], t0);
    osc.frequency.linearRampToValueAtTime(freq[1], t0 + length);
  } else {
    osc.frequency.setValueAtTime(freq, t0);
  }
  g.gain.setValueAtTime(0.0001, t0);
  g.gain.exponentialRampToValueAtTime(gain, t0 + 0.01);
  if (decay) g.gain.exponentialRampToValueAtTime(0.0001, t0 + length);
  else {
    g.gain.setValueAtTime(gain, t0 + Math.max(0.01, length - 0.03));
    g.gain.exponentialRampToValueAtTime(0.0001, t0 + length);
  }
  osc.connect(g).connect(out);
  osc.start(t0);
  osc.stop(t0 + length + 0.05);
};

// Each sound once; returns how long it lasts, in seconds.
const PATTERNS = {
  chime: (ctx, out) => {
    tone(ctx, out, { freq: 880, start: 0, length: 0.6, decay: true, gain: 0.6 });
    tone(ctx, out, { freq: 1318.5, start: 0.18, length: 0.9, decay: true, gain: 0.5 });
    return 1.1;
  },
  bell: (ctx, out) => {
    tone(ctx, out, { freq: 660, start: 0, length: 1.6, decay: true, gain: 0.6 });
    tone(ctx, out, { freq: 1320, start: 0, length: 1.0, decay: true, gain: 0.25 });
    tone(ctx, out, { freq: 1980, start: 0, length: 0.6, decay: true, gain: 0.12 });
    return 1.7;
  },
  beep: (ctx, out) => {
    tone(ctx, out, { freq: 1000, start: 0, length: 0.22, wave: 'square', gain: 0.25 });
    return 0.5;
  },
  double: (ctx, out) => {
    tone(ctx, out, { freq: 1200, start: 0, length: 0.14, wave: 'square', gain: 0.25 });
    tone(ctx, out, { freq: 1200, start: 0.22, length: 0.14, wave: 'square', gain: 0.25 });
    return 0.7;
  },
  alarm: (ctx, out) => {
    tone(ctx, out, { freq: [650, 1250], start: 0, length: 0.55, wave: 'sawtooth', gain: 0.22 });
    tone(ctx, out, { freq: [1250, 650], start: 0.55, length: 0.55, wave: 'sawtooth', gain: 0.22 });
    return 1.15;
  }
};

/**
 * Plays an alert's sound: once, for `seconds`, or until stopped (`forever`,
 * held to ten minutes). Returns a function that stops it.
 */
export const playSound = ({ sound, soundUrl, volume = 1, once = true, seconds = 10, forever = false }) => {
  if (!sound || sound === 'none' || volume <= 0) return () => {};
  const until = Date.now() + (forever ? 10 * 60 : once ? 0 : seconds) * 1000;
  let stopped = false;
  let timer = null;
  let current = null;

  if (sound === 'custom') {
    if (!soundUrl) return () => {};
    const el = new Audio(soundUrl);
    el.volume = Math.min(1, volume);
    const again = () => { if (!stopped && Date.now() < until) { el.currentTime = 0; el.play().catch(() => {}); } };
    el.addEventListener('ended', again);
    el.play().catch(() => {});
    return () => { stopped = true; el.pause(); el.removeEventListener('ended', again); };
  }

  const pattern = PATTERNS[sound] || PATTERNS.chime;
  const ctx = audio();
  const play = () => {
    if (stopped) return;
    const out = ctx.createGain();
    out.gain.value = Math.min(1, volume);
    out.connect(ctx.destination);
    current = out;
    const length = pattern(ctx, out);
    if (Date.now() + length * 1000 < until) timer = setTimeout(play, (length + 0.25) * 1000);
  };
  play();
  return () => {
    stopped = true;
    clearTimeout(timer);
    try { current?.disconnect(); } catch { /* already gone */ }
  };
};
