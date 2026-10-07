// The window's whole view of the main process: a handful of calls and a few
// event streams. Nothing else from Node reaches the page.
const { contextBridge, ipcRenderer } = require('electron');

const call = (channel) => (args) => ipcRenderer.invoke(channel, args);
const on = (channel) => (handler) => {
  const listener = (_e, payload) => handler(payload);
  ipcRenderer.on(channel, listener);
  return () => ipcRenderer.removeListener(channel, listener);
};

contextBridge.exposeInMainWorld('station', {
  info: call('app:info'),
  changelog: call('app:changelog'),
  settings: { get: call('settings:get'), save: call('settings:save'), defaults: call('settings:defaults') },
  scale: {
    status: call('scale:status'), test: call('scale:test'), discover: call('scale:discover'),
    onStatus: on('scale:status'), onWeighed: on('scale:weighed'), onPlate: on('scale:plate'), onDiscoverProgress: on('scale:discover-progress')
  },
  camera: { discover: call('camera:discover'), streams: call('camera:streams'), snapshotTest: call('camera:snapshot-test') },
  plates: { readNow: call('plates:read-now'), status: call('plates:status') },
  history: {
    list: call('history:list'), stats: call('history:stats'), prune: call('history:prune'),
    openFolder: call('history:open-folder'), chooseFolder: call('history:choose-folder'), onEntry: on('history:entry')
  },
  log: { recent: call('log:recent'), onLine: on('log') },
  window: { toggleFullscreen: call('window:toggle-fullscreen'), isFullscreen: call('window:is-fullscreen'), onFullscreen: on('window:fullscreen') },
  updates: { onReady: on('update:ready'), install: call('update:install') }
});
