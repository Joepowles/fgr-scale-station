import React, { useState } from 'react';
import CameraTab from './CameraTab';
import PlatesTab from './PlatesTab';
import ScaleTab from './ScaleTab';
import HistoryTab from './HistoryTab';
import ChangelogTab from './ChangelogTab';
import SupportTab from './SupportTab';
import { btnPrimary, btn } from '../lib';

const TABS = [
  { key: 'camera', label: 'Camera' },
  { key: 'plates', label: 'Plates' },
  { key: 'scale', label: 'Scale' },
  { key: 'history', label: 'History' },
  { key: 'changelog', label: 'Changelog' },
  { key: 'support', label: 'Bug report' }
];

// One window, four tabs, one Save: the same shape as the card settings on the
// Falcon site. Edits are held here until Save, so a tab can be looked at
// without changing anything.
export default function SettingsModal({ settings, info, onSaved, onClose }) {
  const [draft, setDraft] = useState(() => JSON.parse(JSON.stringify(settings)));
  const [tab, setTab] = useState('camera');
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState('');

  const patch = (section, changes) => setDraft((d) => ({ ...d, [section]: { ...d[section], ...changes } }));

  const save = async () => {
    setSaving(true);
    setError('');
    try {
      const next = await window.station.settings.save(draft);
      onSaved(next);
      onClose();
    } catch (err) {
      setError(err.message || 'Could not save');
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className="fixed inset-0 bg-black/70 flex items-center justify-center p-4 z-50">
      <div className="bg-gray-900 border border-gray-700 rounded-lg shadow-2xl w-full max-w-5xl max-h-[92vh] flex flex-col overflow-hidden text-gray-100">
        <div className="flex items-center justify-between px-5 py-3 border-b border-gray-800">
          <div>
            <h2 className="font-semibold">Settings</h2>
            <p className="text-xs text-gray-500">Falcon Scale Station {info.version}</p>
          </div>
          <button type="button" onClick={onClose} className="text-gray-400 hover:text-white px-2">✕</button>
        </div>
        <div className="flex border-b border-gray-800 bg-gray-950 px-2">
          {TABS.map((t) => (
            <button key={t.key} type="button" onClick={() => setTab(t.key)} className={`px-4 py-2.5 text-sm font-medium border-b-2 -mb-px ${tab === t.key ? 'border-emerald-400 text-emerald-300' : 'border-transparent text-gray-400 hover:text-gray-200'}`}>
              {t.label}
            </button>
          ))}
        </div>
        <div className="overflow-y-auto flex-1 p-5">
          {tab === 'camera' && <CameraTab draft={draft} patch={patch} info={info} />}
          {tab === 'plates' && <PlatesTab draft={draft} patch={patch} info={info} />}
          {tab === 'scale' && <ScaleTab draft={draft} patch={patch} info={info} />}
          {tab === 'history' && <HistoryTab draft={draft} patch={patch} info={info} />}
          {tab === 'changelog' && <ChangelogTab draft={draft} patch={patch} info={info} />}
          {tab === 'support' && <SupportTab draft={draft} patch={patch} info={info} />}
        </div>
        <div className="flex items-center justify-between gap-3 px-5 py-3 border-t border-gray-800">
          <span className="text-xs text-red-400">{error}</span>
          <div className="flex gap-2">
            <button type="button" onClick={onClose} className={btn}>Cancel</button>
            <button type="button" onClick={save} disabled={saving} className={btnPrimary}>
              {saving && <div className="w-4 h-4 border-2 border-white/50 border-t-white rounded-full animate-spin" />}
              Save
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
