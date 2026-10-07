import React, { useEffect, useState } from 'react';

// What changed in each version, from the CHANGELOG.md shipped with the app.
// Rendered by hand: headings are versions, dashes are items. Nothing fancier
// is in that file, so nothing fancier is needed here.
export default function ChangelogTab({ info }) {
  const [text, setText] = useState('');
  useEffect(() => { window.station.changelog().then(setText); }, []);

  const sections = [];
  for (const line of text.split(/\r?\n/)) {
    const version = line.match(/^##\s+(.+)$/);
    if (version) { sections.push({ version: version[1].trim(), items: [] }); continue; }
    const item = line.match(/^\s*[-*]\s+(.+)$/);
    if (item && sections.length) sections[sections.length - 1].items.push(item[1].trim());
  }

  return (
    <div className="space-y-5">
      <div className="rounded-md border border-emerald-900 bg-emerald-950/40 p-3">
        <h3 className="text-sm font-semibold text-emerald-200">Changelog</h3>
        <p className="text-xs text-emerald-300/80 mt-0.5">You are on version {info.version}. Updates download on their own when the PC has internet and install from the button in the header.</p>
      </div>
      {!sections.length && <p className="text-sm text-gray-500">No changelog in this build.</p>}
      {sections.map((s) => (
        <section key={s.version} className="border-t border-gray-800 pt-3">
          <h4 className="text-sm font-semibold flex items-center gap-2">
            {s.version}
            {s.version === info.version && <span className="text-[10px] px-1.5 py-0.5 rounded bg-emerald-800 text-emerald-100">this version</span>}
          </h4>
          <ul className="mt-1.5 space-y-1 text-sm text-gray-300 list-disc pl-5">
            {s.items.map((item, i) => <li key={i}>{item}</li>)}
          </ul>
        </section>
      ))}
    </div>
  );
}
