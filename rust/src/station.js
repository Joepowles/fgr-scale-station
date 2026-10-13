// The window's whole view of the Rust side: the same `window.station` the
// Electron preload exposed, rebuilt on Tauri's invoke() and listen(), so the
// components did not have to change.
//
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { getCurrentWindow } from '@tauri-apps/api/window';

// A Rust command that fails rejects with its message as a plain string; the
// components read err.message, as Electron's errors have one.
const asError = (err) => { throw err instanceof Error ? err : new Error(typeof err === 'string' ? err : JSON.stringify(err)); };
const call = (command) => (args) => invoke(command, args === undefined ? {} : { args }).catch(asError);

// listen() resolves to its unlisten function, but the components expect `on`
// to hand one back straight away, so this one cancels whichever comes first.
const on = (event) => (handler) => {
  let unlisten = null;
  let cancelled = false;
  listen(event, (e) => handler(e.payload)).then((fn) => { if (cancelled) fn(); else unlisten = fn; });
  return () => { cancelled = true; if (unlisten) unlisten(); };
};

const win = getCurrentWindow();

window.station = {
  info: call('app_info'),
  changelog: call('app_changelog'),
  copy: call('app_copy'),
  settings: { get: call('settings_get'), save: (patch) => invoke('settings_save', { patch }).catch(asError), defaults: call('settings_defaults') },
  scale: {
    status: call('scale_status'), test: call('scale_test'), discover: call('scale_discover'),
    onStatus: on('scale:status'), onWeighed: on('scale:weighed'), onPlate: on('scale:plate'), onDiscoverProgress: on('scale:discover-progress')
  },
  camera: { discover: call('camera_discover'), reconnect: call('camera_reconnect'), streams: call('camera_streams'), snapshotTest: call('camera_snapshot_test'), onDiscoverProgress: on('camera:discover-progress') },
  plates: { readNow: call('plates_read_now'), status: call('plates_status') },
  history: {
    list: call('history_list'), stats: call('history_stats'), prune: call('history_prune'),
    openFolder: call('history_open_folder'), chooseFolder: call('history_choose_folder'), onEntry: on('history:entry')
  },
  log: { recent: call('log_recent'), onLine: on('log'), write: call('log_write') },
  support: { send: call('support_send'), save: call('support_save'), open: call('support_open') },
  window: {
    toggleFullscreen: async () => { const next = !(await win.isFullscreen()); await win.setFullscreen(next); return next; },
    isFullscreen: () => win.isFullscreen(),
    onFullscreen: on('window:fullscreen')
  },
  updates: { onReady: on('update:ready'), install: call('update_install'), check: call('update_check'), status: call('update_status'), onStatus: on('update:status'), openReleases: call('app_open_releases') }
};
