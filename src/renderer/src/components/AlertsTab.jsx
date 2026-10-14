import React, { useEffect, useState } from 'react';
import { btn, inputClass } from '../lib';
import { SOUNDS, playSound } from '../alertSounds';

// The Alerts tab (Rust app): what to be told of, and how. Each alert is a
// "when" and a "then", written out as a sentence so the list reads plainly;
// open one to change it, Test it to hear it.

const KINDS = [
  { key: 'weightAbove', label: 'Something comes onto the scale', hint: 'As soon as the weight passes the amount below, before it settles. Goes off again once the scale is nearly empty.' },
  { key: 'weighed', label: 'A truck is weighed', hint: 'When the weight settles. Leave the limits empty for every truck, or set them for overweight or light loads.' },
  { key: 'cleared', label: 'The scale is clear again', hint: 'When the truck has driven off.' },
  { key: 'plate', label: 'A plate is read', hint: 'Every plate, only plates on your list, or only plates not on it.' },
  { key: 'noPlate', label: 'No plate could be read', hint: 'When a weighed truck leaves, or its pictures run out, without a plate.' },
  { key: 'unsettled', label: 'A truck sits on the scale without settling', hint: 'It is not weighed until it settles: rocking, idling, or a scale that keeps saying it is moving.' },
  { key: 'motion', label: 'Something moves in the camera picture', hint: 'Watches the small camera stream twice a second while this alert is on.' },
  { key: 'scaleOffline', label: 'The scale stops sending readings', hint: 'Cable, power or the device server.' },
  { key: 'cameraOffline', label: 'The camera stops sending pictures', hint: 'Watches the small camera stream while this alert is on.' }
];
const kindOf = (key) => KINDS.find((k) => k.key === key) || KINDS[1];

const LENGTHS = [
  { key: 'once', label: 'Once' },
  { key: 'seconds', label: 'For a number of seconds' },
  { key: 'dismiss', label: 'Until dismissed' }
];
const DAYS = ['S', 'M', 'T', 'W', 'T', 'F', 'S'];
const DAY_NAMES = ['Sunday', 'Monday', 'Tuesday', 'Wednesday', 'Thursday', 'Friday', 'Saturday'];

const newRule = (kind) => ({
  id: `rule-${Date.now().toString(36)}`,
  name: kindOf(kind).label,
  enabled: true,
  kind,
  threshold: kind === 'weightAbove' ? 500 : 0,
  maxWeight: 0,
  seconds: kind === 'unsettled' ? 30 : 60,
  plateMatch: 'any',
  plates: '',
  sensitivity: 'medium',
  onlyEmptyScale: true,
  sound: kind === 'scaleOffline' || kind === 'cameraOffline' ? 'alarm' : 'chime',
  soundFile: '',
  volume: 100,
  length: 'once',
  lengthSeconds: 10,
  banner: true,
  notify: false,
  cooldownSeconds: kind === 'motion' ? 60 : 10,
  ignoreQuiet: false
});

const num = (v) => (v === '' || v === null || v === undefined ? 0 : Number(v));

// "When a truck is weighed over 80,000 lb → Alarm until dismissed, notification"
const summary = (r, unit) => {
  const w = (n) => `${Number(n).toLocaleString('en-US')} ${unit}`;
  const when = {
    weightAbove: `the scale passes ${w(r.threshold)}`,
    weighed: r.threshold > 0 && r.maxWeight > 0 ? `a truck is weighed between ${w(r.threshold)} and ${w(r.maxWeight)}`
      : r.threshold > 0 ? `a truck is weighed at ${w(r.threshold)} or more`
        : r.maxWeight > 0 ? `a truck is weighed at ${w(r.maxWeight)} or less` : 'a truck is weighed',
    cleared: 'the scale is clear again',
    plate: r.plateMatch === 'listed' ? 'a plate on the list is read' : r.plateMatch === 'unlisted' ? 'a plate not on the list is read' : 'a plate is read',
    noPlate: 'no plate could be read',
    unsettled: `a truck sits ${r.seconds} s without settling`,
    motion: `something moves in the picture${r.onlyEmptyScale ? ' (scale empty)' : ''}`,
    scaleOffline: `no scale readings for ${r.seconds} s`,
    cameraOffline: `no camera pictures for ${r.seconds} s`
  }[r.kind] || r.kind;
  const sound = r.sound === 'none' ? 'no sound' : `${(SOUNDS.find((s) => s.key === r.sound) || SOUNDS[0]).label.replace('…', '')} ${r.length === 'dismiss' ? 'until dismissed' : r.length === 'seconds' ? `for ${r.lengthSeconds} s` : 'once'}`;
  const extras = [r.banner && 'banner', r.notify && 'Windows notification', r.ignoreQuiet && 'even in quiet hours'].filter(Boolean);
  return `When ${when} → ${sound}${extras.length ? `, ${extras.join(', ')}` : ''}`;
};

function Toggle({ checked, onChange, label }) {
  return (
    <button type="button" role="switch" aria-checked={checked} aria-label={label} onClick={() => onChange(!checked)}
      className={`relative inline-flex h-5 w-9 shrink-0 rounded-full transition-colors ${checked ? 'bg-emerald-500' : 'bg-gray-600'}`}>
      <span className={`absolute top-0.5 h-4 w-4 rounded-full bg-white transition-transform ${checked ? 'translate-x-4' : 'translate-x-0.5'}`} />
    </button>
  );
}

const Field = ({ label, children, hint }) => (
  <label className="block text-xs text-gray-400">{label}
    <div className="mt-1">{children}</div>
    {hint && <span className="text-[11px] text-gray-500">{hint}</span>}
  </label>
);

function RuleEditor({ rule, unit, onChange }) {
  const set = (changes) => onChange({ ...rule, ...changes });
  const k = kindOf(rule.kind);
  const chooseFile = async () => {
    const path = await window.station.alerts.chooseSound();
    if (path) set({ sound: 'custom', soundFile: path });
  };
  return (
    <div className="grid grid-cols-1 md:grid-cols-2 gap-5 pt-3">
      <div className="space-y-3">
        <h4 className="text-xs font-semibold uppercase tracking-wide text-gray-400">When</h4>
        <Field label="Name"><input className={inputClass} value={rule.name} onChange={(e) => set({ name: e.target.value })} /></Field>
        <Field label="This happens" hint={k.hint}>
          <select className={inputClass} value={rule.kind} onChange={(e) => set({ kind: e.target.value })}>
            {KINDS.map((x) => <option key={x.key} value={x.key}>{x.label}</option>)}
          </select>
        </Field>
        {rule.kind === 'weightAbove' && (
          <Field label={`Weight over (${unit})`} hint="The history only counts trucks over the Scale tab's minimum weight; this can be much lower.">
            <input type="number" min={0} step={100} className={inputClass} value={rule.threshold} onChange={(e) => set({ threshold: num(e.target.value) })} />
          </Field>
        )}
        {rule.kind === 'weighed' && (
          <div className="grid grid-cols-2 gap-3">
            <Field label={`Only at or over (${unit})`}><input type="number" min={0} step={100} className={inputClass} value={rule.threshold || ''} placeholder="any" onChange={(e) => set({ threshold: num(e.target.value) })} /></Field>
            <Field label={`Only at or under (${unit})`}><input type="number" min={0} step={100} className={inputClass} value={rule.maxWeight || ''} placeholder="any" onChange={(e) => set({ maxWeight: num(e.target.value) })} /></Field>
          </div>
        )}
        {rule.kind === 'plate' && (
          <>
            <Field label="Which plates">
              <select className={inputClass} value={rule.plateMatch} onChange={(e) => set({ plateMatch: e.target.value })}>
                <option value="any">Any plate</option>
                <option value="listed">Only plates on the list</option>
                <option value="unlisted">Only plates not on the list (trucks you do not know)</option>
              </select>
            </Field>
            {rule.plateMatch !== 'any' && (
              <Field label="The list" hint="Separated by commas or spaces. Dashes and spaces inside a plate do not matter.">
                <textarea className={`${inputClass} mono min-h-[70px]`} value={rule.plates} onChange={(e) => set({ plates: e.target.value })} placeholder="44816R, 223647H" />
              </Field>
            )}
          </>
        )}
        {['unsettled', 'scaleOffline', 'cameraOffline'].includes(rule.kind) && (
          <Field label="For how long (seconds)"><input type="number" min={5} step={5} className={inputClass} value={rule.seconds} onChange={(e) => set({ seconds: num(e.target.value) })} /></Field>
        )}
        {rule.kind === 'motion' && (
          <>
            <Field label="How much movement" hint="Low: a vehicle or a person close by. High: any small movement - may go off for rain, shadows or a waving flag.">
              <select className={inputClass} value={rule.sensitivity} onChange={(e) => set({ sensitivity: e.target.value })}>
                <option value="low">A lot (low sensitivity)</option>
                <option value="medium">Some (medium)</option>
                <option value="high">A little (high sensitivity)</option>
              </select>
            </Field>
            <label className="flex items-center gap-2 text-xs text-gray-300"><input type="checkbox" checked={rule.onlyEmptyScale} onChange={(e) => set({ onlyEmptyScale: e.target.checked })} /> Only while the scale is empty</label>
          </>
        )}
      </div>
      <div className="space-y-3">
        <h4 className="text-xs font-semibold uppercase tracking-wide text-gray-400">Then</h4>
        <div className="grid grid-cols-2 gap-3">
          <Field label="Sound">
            <select className={inputClass} value={rule.sound} onChange={(e) => (e.target.value === 'custom' && !rule.soundFile ? chooseFile() : set({ sound: e.target.value }))}>
              {SOUNDS.map((x) => <option key={x.key} value={x.key}>{x.label}</option>)}
            </select>
          </Field>
          <Field label="Play it">
            <select className={inputClass} value={rule.length} onChange={(e) => set({ length: e.target.value })}>
              {LENGTHS.map((x) => <option key={x.key} value={x.key}>{x.label}</option>)}
            </select>
          </Field>
        </div>
        {rule.sound === 'custom' && (
          <div className="flex items-center gap-2 text-xs text-gray-400">
            <span className="mono truncate flex-1" title={rule.soundFile}>{rule.soundFile || 'No file chosen'}</span>
            <button type="button" className={btn} onClick={chooseFile}>Choose…</button>
          </div>
        )}
        {rule.length === 'seconds' && (
          <Field label="For (seconds)"><input type="number" min={1} max={600} className={inputClass} value={rule.lengthSeconds} onChange={(e) => set({ lengthSeconds: num(e.target.value) })} /></Field>
        )}
        <Field label={<>Volume: <span className="text-gray-200">{Math.round(rule.volume)}%</span></>}>
          <input type="range" min={0} max={100} value={rule.volume} onChange={(e) => set({ volume: Number(e.target.value) })} className="w-full" />
        </Field>
        <label className="flex items-center gap-2 text-xs text-gray-300"><input type="checkbox" checked={rule.banner} onChange={(e) => set({ banner: e.target.checked })} /> Show a banner over the picture</label>
        <label className="flex items-center gap-2 text-xs text-gray-300"><input type="checkbox" checked={rule.notify} onChange={(e) => set({ notify: e.target.checked })} /> Windows notification (seen even with the app behind other windows)</label>
        <label className="flex items-center gap-2 text-xs text-gray-300"><input type="checkbox" checked={rule.ignoreQuiet} onChange={(e) => set({ ignoreQuiet: e.target.checked })} /> Sound even in quiet hours</label>
        <Field label="Not again within (seconds)" hint="So one event does not set it off over and over. 0: every time.">
          <input type="number" min={0} step={5} className={inputClass} value={rule.cooldownSeconds} onChange={(e) => set({ cooldownSeconds: num(e.target.value) })} />
        </Field>
      </div>
    </div>
  );
}

export default function AlertsTab({ draft, patch }) {
  const a = draft.alerts;
  const unit = draft.scale?.unit === 'kg' ? 'kg' : 'lb';
  const [open, setOpen] = useState(null);
  const [adding, setAdding] = useState('weightAbove');
  const [mutedUntil, setMutedUntil] = useState(null);
  const set = (changes) => patch('alerts', changes);
  const setQuiet = (changes) => set({ quiet: { ...a.quiet, ...changes } });
  const setRule = (i, rule) => set({ rules: a.rules.map((r, j) => (j === i ? rule : r)) });

  useEffect(() => {
    window.station.alerts.status().then((s) => setMutedUntil(s?.mutedUntil || null)).catch(() => {});
    return window.station.alerts.onStatus((s) => setMutedUntil(s?.mutedUntil || null));
  }, []);
  const muted = mutedUntil && mutedUntil > Date.now();

  const add = () => {
    const rule = newRule(adding);
    set({ rules: [...a.rules, rule] });
    setOpen(rule.id);
  };
  // Heard here and now, from the rule as it stands on screen (saved or not):
  // the app decides quiet hours and mute as it would for the real thing.
  const test = (rule) => window.station.alerts.test(rule);

  return (
    <div className="space-y-5">
      <div className="flex items-start justify-between gap-4 rounded-md border border-emerald-900 bg-emerald-950/40 p-3">
        <div>
          <h3 className="text-sm font-semibold text-emerald-200">Alerts</h3>
          <p className="text-xs text-emerald-300/80 mt-0.5">Sounds, banners and Windows notifications for what happens at the scale. Add as many as you need; each can be tested here. Changes apply when you press Save.</p>
        </div>
        <label className="inline-flex items-center gap-2 text-sm whitespace-nowrap"><Toggle checked={a.enabled} onChange={(v) => set({ enabled: v })} label="Alerts on" /> Alerts on</label>
      </div>

      <section className="grid grid-cols-1 md:grid-cols-3 gap-5">
        <div className="space-y-3">
          <Field label={<>Overall volume: <span className="text-gray-200">{Math.round(a.volume)}%</span></>}>
            <input type="range" min={0} max={100} value={a.volume} onChange={(e) => set({ volume: Number(e.target.value) })} className="w-full" />
          </Field>
          <button type="button" className={btn} onClick={() => playSound({ sound: 'chime', volume: a.volume / 100 })}>▶ Hear the volume</button>
        </div>
        <div className="space-y-2.5">
          <label className="flex items-center gap-2 text-xs text-gray-300"><Toggle checked={a.notifications} onChange={(v) => set({ notifications: v })} label="Windows notifications" /> Windows notifications</label>
          <label className="flex items-center gap-2 text-xs text-gray-300"><Toggle checked={a.trayIcon} onChange={(v) => set({ trayIcon: v })} label="Tray icon" /> Icon in the system tray</label>
          <p className="text-[11px] text-gray-500">The tray icon brings the window forward, and can mute alerts for an hour.</p>
        </div>
        <div className="space-y-2">
          <div className="text-xs text-gray-400">{muted ? `Muted until ${new Date(mutedUntil).toLocaleTimeString([], { hour: 'numeric', minute: '2-digit' })}` : 'Not muted'}</div>
          <div className="flex gap-2 flex-wrap">
            {muted
              ? <button type="button" className={btn} onClick={() => window.station.alerts.mute(0)}>Unmute</button>
              : [15, 60, 240].map((m) => <button key={m} type="button" className={btn} onClick={() => window.station.alerts.mute(m)}>Mute {m < 60 ? `${m} min` : `${m / 60} h`}</button>)}
          </div>
          <p className="text-[11px] text-gray-500">Muting silences sounds and notifications straight away; banners still show.</p>
        </div>
      </section>

      <section className="rounded-md border border-gray-800 p-3 space-y-3">
        <div className="flex items-center justify-between gap-3">
          <div>
            <h3 className="text-sm font-semibold">Quiet hours</h3>
            <p className="text-[11px] text-gray-500">No alert sounds at these times, unless an alert is set to sound even then.</p>
          </div>
          <Toggle checked={a.quiet.enabled} onChange={(v) => setQuiet({ enabled: v })} label="Quiet hours" />
        </div>
        {a.quiet.enabled && (
          <div className="grid grid-cols-1 md:grid-cols-3 gap-4">
            <div className="grid grid-cols-2 gap-2">
              <Field label="From"><input type="time" className={inputClass} value={a.quiet.from} onChange={(e) => setQuiet({ from: e.target.value })} /></Field>
              <Field label="To"><input type="time" className={inputClass} value={a.quiet.to} onChange={(e) => setQuiet({ to: e.target.value })} /></Field>
            </div>
            <Field label="On" hint={a.quiet.days.length ? 'Hours past midnight belong to the day they start.' : 'Every day. Pick days to limit it.'}>
              <div className="flex gap-1">
                {DAYS.map((d, i) => {
                  const on = a.quiet.days.includes(i);
                  return (
                    <button key={i} type="button" title={DAY_NAMES[i]} onClick={() => setQuiet({ days: on ? a.quiet.days.filter((x) => x !== i) : [...a.quiet.days, i].sort() })}
                      className={`w-8 h-8 rounded text-xs border ${on ? 'bg-gray-100 text-black border-gray-100' : 'bg-gray-800 text-gray-300 border-gray-600'}`}>{d}</button>
                  );
                })}
              </div>
            </Field>
            <Field label="During them">
              <select className={inputClass} value={a.quiet.mode} onChange={(e) => setQuiet({ mode: e.target.value })}>
                <option value="sound">Silence sounds; still show banners and notifications</option>
                <option value="all">Silence everything; only the log keeps them</option>
              </select>
            </Field>
          </div>
        )}
      </section>

      <section className="space-y-2">
        <div className="flex items-center justify-between gap-3 flex-wrap">
          <h3 className="text-sm font-semibold">Your alerts</h3>
          <div className="flex gap-2">
            <select className={`${inputClass} w-auto`} value={adding} onChange={(e) => setAdding(e.target.value)} aria-label="Kind of alert to add">
              {KINDS.map((x) => <option key={x.key} value={x.key}>{x.label}</option>)}
            </select>
            <button type="button" className={`${btn} whitespace-nowrap`} onClick={add}>+ Add alert</button>
          </div>
        </div>
        {!a.rules.length && <p className="text-xs text-gray-500">No alerts yet. Pick a kind and press Add alert.</p>}
        {a.rules.map((r, i) => (
          <div key={r.id} className={`rounded-md border px-3 py-2 ${r.enabled ? 'border-gray-700 bg-gray-900' : 'border-gray-800 bg-gray-950 opacity-70'}`}>
            <div className="flex items-center gap-3">
              <Toggle checked={r.enabled} onChange={(v) => setRule(i, { ...r, enabled: v })} label={`${r.name} on`} />
              <button type="button" className="flex-1 min-w-0 text-left" onClick={() => setOpen(open === r.id ? null : r.id)}>
                <div className="text-sm font-medium truncate">{r.name || kindOf(r.kind).label}</div>
                <div className="text-[11px] text-gray-400 truncate">{summary(r, unit)}</div>
              </button>
              <button type="button" className="px-2 py-1 text-xs rounded bg-gray-800 hover:bg-gray-700" onClick={() => test(r)} title="Hear and see it now">Test</button>
              <button type="button" className="px-2 py-1 text-xs rounded bg-gray-800 hover:bg-gray-700" onClick={() => setOpen(open === r.id ? null : r.id)}>{open === r.id ? 'Close' : 'Edit'}</button>
              <button type="button" className="px-2 py-1 text-xs rounded bg-gray-800 hover:bg-red-900" onClick={() => set({ rules: a.rules.filter((_, j) => j !== i) })} title="Delete this alert">✕</button>
            </div>
            {open === r.id && <RuleEditor rule={r} unit={unit} onChange={(rule) => setRule(i, rule)} />}
          </div>
        ))}
      </section>
    </div>
  );
}
