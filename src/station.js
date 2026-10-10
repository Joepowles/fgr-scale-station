// The window's whole view of the Rust side: the same `window.station` the
// Electron preload exposed, rebuilt on Tauri's invoke() and listen(), so the
// components did not have to change.
//
// Calls the Rust side does not have yet reject with "not ported yet"; the few
// the window makes on startup answer with something harmless instead.
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { getCurrentWindow } from '@tauri-apps/api/window';

const call = (command) => (args) => invoke(command, args === undefined ? {} : { args });

// listen() resolves to its unlisten function, but the components expect `on`
// to hand one back straight away, so this one cancels whichever comes first.
const on = (event) => (handler) => {
  let unlisten = null;
  let cancelled = false;
  listen(event, (e) => handler(e.payload)).then((fn) => { if (cancelled) fn(); else unlisten = fn; });
  return () => { cancelled = true; if (unlisten) unlisten(); };
};

const notYet = (what) => () => Promise.reject(new Error(`${what} is not ported to Rust yet`));
const answer = (value) => () => Promise.resolve(value);
const never = () => () => {};

const win = getCurrentWindow();

window.station = {
  info: call('app_info'),
  changelog: call('app_changelog'),
  settings: { get: call('settings_get'), save: (patch) => invoke('settings_save', { patch }), defaults: call('settings_defaults') },
  scale: {
    status: call('scale_status'), test: call('scale_test'), discover: call('scale_discover'),
    onStatus: on('scale:status'), onWeighed: on('scale:weighed'), onPlate: on('scale:plate'), onDiscoverProgress: on('scale:discover-progress')
  },
  camera: { discover: notYet('Camera discovery'), streams: notYet('Camera streams'), snapshotTest: notYet('Camera snapshots'), onDiscoverProgress: never() },
  plates: { readNow: notYet('The plate reader'), status: answer({ installed: false, ready: false, lastError: 'not ported to Rust yet' }) },
  history: {
    list: call('history_list'), stats: call('history_stats'), prune: call('history_prune'),
    openFolder: call('history_open_folder'), chooseFolder: call('history_choose_folder'), onEntry: on('history:entry')
  },
  log: { recent: call('log_recent'), onLine: on('log') },
  support: { send: notYet('Bug reports'), save: notYet('Bug reports'), open: notYet('Bug reports') },
  window: {
    toggleFullscreen: async () => { const next = !(await win.isFullscreen()); await win.setFullscreen(next); return next; },
    isFullscreen: () => win.isFullscreen(),
    onFullscreen: never()
  },
  updates: { onReady: never(), install: answer(false), check: answer({ state: 'dev', message: 'Updates are not ported to Rust yet' }), onStatus: never() }
};
