// Small shared helpers for the window.
export const fmtWeight = (value, unit = 'lb') => (Number.isFinite(Number(value))
  ? `${Number(value).toLocaleString('en-US')} ${unit === 'kg' ? 'kg' : 'lb'}`
  : '— —');

export const timeAgo = (value) => {
  if (!value) return 'never';
  const seconds = Math.round((Date.now() - new Date(value).getTime()) / 1000);
  if (seconds < 5) return 'just now';
  if (seconds < 60) return `${seconds}s ago`;
  if (seconds < 3600) return `${Math.round(seconds / 60)} min ago`;
  if (seconds < 86400) return `${Math.round(seconds / 3600)} h ago`;
  return new Date(value).toLocaleString();
};

export const fmtTime = (value) => new Date(value).toLocaleString([], { month: 'short', day: 'numeric', hour: 'numeric', minute: '2-digit', second: '2-digit' });

export const CORNERS = [
  { key: 'top-left', label: 'Top left' },
  { key: 'top-right', label: 'Top right' },
  { key: 'bottom-left', label: 'Bottom left' },
  { key: 'bottom-right', label: 'Bottom right' }
];

export const cornerClass = (corner) => ({
  'top-left': 'top-3 left-3 items-start',
  'top-right': 'top-3 right-3 items-end',
  'bottom-left': 'bottom-3 left-3 items-start flex-col-reverse',
  'bottom-right': 'bottom-3 right-3 items-end flex-col-reverse'
}[corner] || 'bottom-3 left-3 items-start flex-col-reverse');

export const inputClass = 'w-full bg-gray-800 border border-gray-600 rounded-md px-2 py-1.5 text-sm text-gray-100 focus:outline-none focus:border-emerald-400';
export const btn = 'px-3 py-1.5 text-sm rounded-md bg-gray-700 hover:bg-gray-600 text-gray-100 disabled:opacity-40';
export const btnPrimary = 'px-4 py-2 text-sm rounded-md bg-emerald-600 hover:bg-emerald-500 text-white disabled:opacity-40 flex items-center gap-2';
