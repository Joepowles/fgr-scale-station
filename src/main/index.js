// Falcon Scale Station - main process.
//
// One window, one camera, one scale. The services here are the Falcon
// agent's, run locally: the scale reader raises a weighing, the pipeline
// photographs and reads the plate, the history keeps it. The window talks to
// all of it over IPC, and gets the camera picture from a local HTTP server
// (fragmented MP4 from ffmpeg) because <video> wants a URL, not a buffer.
const { app, BrowserWindow, ipcMain, dialog, shell, Menu } = require('electron');
const path = require('path');
const http = require('http');
const fs = require('fs');
const settingsStore = require('./settings');
const paths = require('./paths');
const { ScaleReader, testConnection } = require('./services/scaleReader');
const { History } = require('./services/history');
const { WeighingPipeline } = require('./services/weighingPipeline');
const bugReport = require('./services/bugReport');
const cameraStream = require('./services/cameraStream');
const cameraDiscovery = require('./services/cameraDiscovery');
const nportDiscovery = require('./services/nportDiscovery');
const detector = require('./services/vehicleDetector');
const plateReader = require('./services/plateReader');
const { ffmpegAvailable } = require('./services/ffmpegPath');
const { captureRtspSnapshot, rtspInjectCredentials } = require('./services/rtspSnapshot');

let win = null;
let updater = null;
let server = null;
let serverPort = 0;
let scale = null;
let history = null;
let pipeline = null;
const logLines = [];

// The log also goes to station.log beside the settings file, so what the
// scale and camera did on a day can be read back on the yard PC afterwards.
// Kept to about 2 MB: the older half is dropped when it grows past that.
const LOG_FILE_MAX = 2 * 1024 * 1024;
const logFile = () => path.join(paths.userDataDir(), 'station.log');
const appendLogFile = (line) => {
  try {
    const file = logFile();
    fs.appendFileSync(file, `${line}\n`);
    if (fs.statSync(file).size > LOG_FILE_MAX) {
      const text = fs.readFileSync(file, 'utf8');
      fs.writeFileSync(file, text.slice(text.indexOf('\n', text.length / 2) + 1));
    }
  } catch {}
};
const log = (message) => {
  const line = `${new Date().toISOString()} ${message}`;
  logLines.push(line);
  if (logLines.length > 500) logLines.shift();
  console.log(line);
  appendLogFile(line);
  send('log', line);
};

const send = (channel, payload) => {
  if (win && !win.isDestroyed()) win.webContents.send(channel, payload);
};

const settings = () => settingsStore.load();

// ── Camera stream server ──────────────────────────────────────────────────
// http://127.0.0.1:<port>/stream?hd=1  fragmented MP4 of the chosen stream
// http://127.0.0.1:<port>/snapshot     one JPEG from the main stream
// http://127.0.0.1:<port>/photo/<name> a history photo
const streamUrlFor = (hd) => {
  const c = settings().camera;
  const url = hd ? (c.hdStreamUrl || c.streamUrl) : c.streamUrl;
  return url ? rtspInjectCredentials(url, c.username, c.password) : '';
};

const startServer = () => new Promise((resolve) => {
  server = http.createServer(async (req, res) => {
    const url = new URL(req.url, 'http://127.0.0.1');
    try {
      if (url.pathname === '/stream') {
        const rtsp = streamUrlFor(url.searchParams.get('hd') === '1');
        if (!rtsp) { res.writeHead(404); return res.end('no camera'); }
        if (!ffmpegAvailable()) { res.writeHead(503); return res.end('ffmpeg missing'); }
        return cameraStream.streamRtspToFmp4(rtsp, res);
      }
      if (url.pathname === '/snapshot') {
        const rtsp = streamUrlFor(url.searchParams.get('hd') === '1');
        if (!rtsp) { res.writeHead(404); return res.end('no camera'); }
        const jpeg = await captureRtspSnapshot(rtsp, null, null, 12000);
        res.writeHead(200, { 'Content-Type': 'image/jpeg', 'Cache-Control': 'no-store' });
        return res.end(jpeg);
      }
      if (url.pathname.startsWith('/photo/')) {
        const name = path.basename(decodeURIComponent(url.pathname.slice(7)));
        const file = history?.photoPath(name);
        if (!file || !fs.existsSync(file)) { res.writeHead(404); return res.end(); }
        res.writeHead(200, { 'Content-Type': 'image/jpeg', 'Cache-Control': 'no-store' });
        return fs.createReadStream(file).pipe(res);
      }
      res.writeHead(404); res.end();
    } catch (err) {
      res.writeHead(500); res.end(String(err.message || err));
    }
  });
  server.listen(0, '127.0.0.1', () => { serverPort = server.address().port; resolve(serverPort); });
});

// ── Scale ─────────────────────────────────────────────────────────────────
const startScale = () => {
  const previous = scale;
  if (scale) { scale.stop(); scale = null; }
  const s = settings().scale;
  if (!s.enabled || !s.host) { send('scale:status', null); return; }
  scale = new ScaleReader(s).adopt(previous).start();
  scale.on('status', (status) => send('scale:status', status));
  scale.on('weighed', async (weighing) => {
    log(`weighed ${weighing.weight} ${weighing.unit}`);
    send('scale:weighed', weighing);
    const entry = await pipeline.onWeighed(weighing);
    if (entry) send('history:entry', entry);
  });
  scale.on('reweighed', async (weighing) => {
    log(`reweighed ${weighing.weight} ${weighing.unit} (was ${weighing.previous})`);
    send('scale:weighed', weighing);
    const entry = await pipeline.onReweighed(weighing);
    if (entry) send('history:entry', entry);
  });
  scale.on('cleared', () => { log('deck clear'); pipeline?.onCleared(); });
  log(`reading the scale at ${s.host}:${s.port}`);
};

const startHistory = () => {
  const h = settings().history;
  const folder = settingsStore.historyFolder();
  if (!history) history = new History({ folder, retentionDays: h.retentionDays, savePhotos: h.savePhotos, saveLog: h.saveLog }).start();
  else history.configure({ folder, retentionDays: h.retentionDays, savePhotos: h.savePhotos, saveLog: h.saveLog });
  history.removeAllListeners('entry');
  history.on('entry', (entry) => send('history:entry', entry));
};

const applySettings = () => {
  startHistory();
  startScale();
  cameraStream.stopAllCameraStreams();
  if (settings().plates.enabled) plateReader.warmUp();
  detector.warmUp().catch((err) => log(`vehicle model: ${err.message}`));
  if (win) {
    win.setAlwaysOnTop(!!settings().window.alwaysOnTop);
    if (settings().window.fullscreen !== win.isFullScreen()) win.setFullScreen(!!settings().window.fullscreen);
  }
  app.setLoginItemSettings({ openAtLogin: !!settings().window.startWithWindows });
};

// ── IPC ───────────────────────────────────────────────────────────────────
const registerIpc = () => {
  ipcMain.handle('app:info', () => ({
    version: app.getVersion(), serverPort, ffmpeg: ffmpegAvailable(), plateReader: plateReader.status(),
    vehicleModel: detector.modelIsPresent(), userData: paths.userDataDir(), settingsFile: settingsStore.file()
  }));
  ipcMain.handle('settings:get', () => settings());
  ipcMain.handle('settings:save', (_e, patch) => { const next = settingsStore.save(patch); applySettings(); return next; });
  ipcMain.handle('settings:defaults', () => settingsStore.DEFAULTS);
  ipcMain.handle('scale:status', () => scale?.status || null);
  ipcMain.handle('scale:test', (_e, args) => testConnection(args));
  ipcMain.handle('scale:discover', async () => nportDiscovery.discover({ onProgress: (done, total) => send('scale:discover-progress', { done, total }) }));
  ipcMain.handle('camera:discover', (_e, args) => cameraDiscovery.discover({ ...(args || {}), onProgress: (done, total, network) => send('camera:discover-progress', { done, total, network }) }));
  ipcMain.handle('camera:streams', (_e, args) => cameraDiscovery.streamsOf(args));
  ipcMain.handle('camera:snapshot-test', async (_e, { url, username, password }) => {
    const jpeg = await captureRtspSnapshot(url, username, password, 12000);
    const sharp = require('sharp');
    const meta = await sharp(jpeg).metadata();
    return { width: meta.width, height: meta.height, bytes: jpeg.length, image: `data:image/jpeg;base64,${jpeg.toString('base64')}` };
  });
  ipcMain.handle('plates:read-now', async () => {
    const r = await pipeline.readPlateNow();
    return { ...r, image: r.image ? `data:image/jpeg;base64,${r.image.toString('base64')}` : null, reader: plateReader.status() };
  });
  ipcMain.handle('plates:status', () => plateReader.status());
  ipcMain.handle('history:list', (_e, args) => history.list(args || {}));
  ipcMain.handle('history:stats', () => history.stats());
  ipcMain.handle('history:prune', () => history.prune());
  ipcMain.handle('history:open-folder', () => shell.openPath(history.folder));
  ipcMain.handle('history:choose-folder', async () => {
    const r = await dialog.showOpenDialog(win, { properties: ['openDirectory', 'createDirectory'], title: 'Where to keep the history log and photos' });
    return r.canceled ? null : r.filePaths[0];
  });
  ipcMain.handle('log:recent', () => logLines.slice(-200));
  // The Bug report tab. The report is what was typed plus the app's own
  // account of itself: version, settings without passwords, the last
  // weighings, the tail of the log. Filed on GitHub, or saved to a file to
  // send some other way when the PC has no internet.
  const buildBugReport = (description) => {
    let logText = '';
    try { logText = fs.readFileSync(logFile(), 'utf8'); } catch { logText = logLines.join('\n'); }
    return bugReport.buildReport({
      description,
      info: { version: app.getVersion(), electron: process.versions.electron, ffmpeg: ffmpegAvailable(), plateReader: plateReader.status(), vehicleModel: detector.modelIsPresent() },
      settings: settings(), log: logText, history: history ? history.list({ limit: 40 }) : [], scale: scale?.status || null
    });
  };
  ipcMain.handle('support:send', async (_e, { description, repo, token } = {}) => {
    const report = buildBugReport(description);
    const sup = settings().support || {};
    const issue = await bugReport.fileIssue({ repo: repo || sup.repo, token: token || sup.githubToken, title: report.title, body: report.body });
    log(`bug report filed: ${issue.url}`);
    return issue;
  });
  ipcMain.handle('support:save', async (_e, { description } = {}) => {
    const report = buildBugReport(description);
    const stampText = new Date().toISOString().slice(0, 19).replace(/[:T]/g, '-');
    const r = await dialog.showSaveDialog(win, { title: 'Save the bug report', defaultPath: path.join(app.getPath('desktop'), `scale-station-report-${stampText}.md`), filters: [{ name: 'Markdown', extensions: ['md'] }] });
    if (r.canceled || !r.filePath) return null;
    fs.writeFileSync(r.filePath, `# ${report.title}\n\n${report.body}`);
    return r.filePath;
  });
  ipcMain.handle('support:open', (_e, url) => { if (/^https:\/\/github\.com\//.test(String(url))) shell.openExternal(url); });
  ipcMain.handle('app:changelog', () => {
    for (const candidate of [path.join(paths.resourcesDir(), 'CHANGELOG.md'), path.join(paths.projectRoot, 'CHANGELOG.md')]) {
      try { return fs.readFileSync(candidate, 'utf8'); } catch {}
    }
    return '';
  });
  ipcMain.handle('window:toggle-fullscreen', () => { win.setFullScreen(!win.isFullScreen()); return win.isFullScreen(); });
  ipcMain.handle('window:is-fullscreen', () => win.isFullScreen());
  // The downloaded update is applied by quitting into the installer, which
  // relaunches the app when done. Without this it happens on the next quit.
  ipcMain.handle('update:install', () => { if (updater) { setImmediate(() => updater.quitAndInstall(true, true)); return true; } return false; });
  // The Changelog tab's button. Progress and the outcome arrive on update:status.
  ipcMain.handle('update:check', async () => {
    if (!updater) return { state: app.isPackaged ? 'error' : 'dev', message: app.isPackaged ? 'Updater not available' : 'Updates only apply to the installed app' };
    try {
      const result = await updater.checkForUpdates();
      const version = result?.updateInfo?.version;
      const newer = !!version && version !== app.getVersion();
      return { state: newer ? 'downloading' : 'none', version: version || app.getVersion() };
    } catch (err) {
      return { state: 'error', message: err.message };
    }
  });
};

// ── Window ────────────────────────────────────────────────────────────────
const createWindow = () => {
  win = new BrowserWindow({
    width: 1280,
    height: 800,
    minWidth: 640,
    minHeight: 420,
    backgroundColor: '#000000',
    autoHideMenuBar: true,
    title: 'Falcon Scale Station',
    icon: path.join(paths.resourcesDir(), '..', 'build', 'icon.png'),
    webPreferences: {
      preload: path.join(__dirname, '../preload/index.js'),
      contextIsolation: true,
      nodeIntegration: false,
      sandbox: false
    }
  });
  Menu.setApplicationMenu(null);
  win.on('enter-full-screen', () => send('window:fullscreen', true));
  win.on('leave-full-screen', () => send('window:fullscreen', false));
  if (process.env.ELECTRON_RENDERER_URL) win.loadURL(process.env.ELECTRON_RENDERER_URL);
  else win.loadFile(path.join(__dirname, '../renderer/index.html'));
};

const startUpdater = () => {
  if (!app.isPackaged) return;
  try {
    const { autoUpdater } = require('electron-updater');
    updater = autoUpdater;
    autoUpdater.autoDownload = true;
    autoUpdater.autoInstallOnAppQuit = true;
    autoUpdater.on('checking-for-update', () => send('update:status', { state: 'checking' }));
    autoUpdater.on('update-not-available', (info) => send('update:status', { state: 'none', version: info?.version || app.getVersion() }));
    autoUpdater.on('update-available', (info) => { log(`update ${info.version} found, downloading`); send('update:status', { state: 'downloading', version: info.version }); });
    autoUpdater.on('download-progress', (p) => send('update:status', { state: 'downloading', percent: Math.round(p.percent || 0) }));
    autoUpdater.on('update-downloaded', (info) => { log(`update ${info.version} ready; installs on next restart`); send('update:ready', info.version); send('update:status', { state: 'ready', version: info.version }); });
    const check = () => autoUpdater.checkForUpdates().catch(() => {});
    // A yard PC is often on a poor link, and a download that drops is thrown
    // away rather than resumed. Rather than wait for the six-hourly check,
    // try again soon, backing off up to an hour. The download is differential
    // (only the blocks that changed since the installed version), so each
    // attempt is a small fraction of the installer.
    const RETRY_MINUTES = [2, 5, 15, 30, 60];
    let failures = 0;
    let retryTimer = null;
    const clearRetry = () => { failures = 0; if (retryTimer) { clearTimeout(retryTimer); retryTimer = null; } };
    autoUpdater.on('update-not-available', clearRetry);
    autoUpdater.on('update-downloaded', clearRetry);
    autoUpdater.on('error', (err) => {
      const minutes = RETRY_MINUTES[Math.min(failures, RETRY_MINUTES.length - 1)];
      failures += 1;
      log(`updater: ${err.message}; trying again in ${minutes} min`);
      send('update:status', { state: 'error', message: err.message, retryMinutes: minutes });
      if (retryTimer) clearTimeout(retryTimer);
      retryTimer = setTimeout(() => { retryTimer = null; check(); }, minutes * 60 * 1000);
    });
    setTimeout(check, 30000);
    setInterval(check, 6 * 60 * 60 * 1000);
  } catch (err) {
    log(`updater unavailable: ${err.message}`);
  }
};

app.whenReady().then(async () => {
  await startServer();
  registerIpc();
  createWindow();
  applySettings();
  startHistory();
  pipeline = new WeighingPipeline({
    settings, history,
    isLoaded: () => !!scale?.loaded,
    onPlate: (plate) => { scale?.recordPlate(plate); send('scale:plate', plate); },
    log
  });
  startUpdater();
  log(`station ${app.getVersion()} started; ffmpeg ${ffmpegAvailable() ? 'found' : 'MISSING'}; plate reader ${plateReader.isInstalled() ? 'bundled' : 'MISSING'}`);
});

app.on('window-all-closed', () => app.quit());
app.on('before-quit', () => {
  scale?.stop();
  history?.stop();
  cameraStream.stopAllCameraStreams();
  plateReader.stop();
  server?.close();
});
