// Where things live, in development and once installed.
//
// Installed, electron-builder puts the models, the plate reader and ffmpeg
// under the app's resources directory (see
// electron-builder.yml extraResources). In development they are the same
// folders under ./resources. Everything the app writes - settings, the
// history log and its photos - goes under the user's app data folder, or
// wherever the History tab points.
const path = require('path');
const fs = require('fs');

let electronApp = null;
try { electronApp = require('electron').app; } catch { /* tests, scripts */ }

const projectRoot = path.resolve(__dirname, '..', '..');

const resourcesDir = () => (electronApp && electronApp.isPackaged
  ? process.resourcesPath
  : path.join(projectRoot, 'resources'));

const userDataDir = () => {
  if (electronApp) return electronApp.getPath('userData');
  const base = process.env.LOCALAPPDATA || process.env.XDG_DATA_HOME || path.join(require('os').homedir(), '.local', 'share');
  return path.join(base, 'fgr-scale-station');
};

const ensureDir = (dir) => { fs.mkdirSync(dir, { recursive: true }); return dir; };

const modelPath = () => path.join(resourcesDir(), 'models', 'yolov10n.onnx');
// The plate reader (plate-reader/, built by scripts/build-plate-reader.js)
// sits beside ffmpeg, and reads its two models from plate-models.
const plateReaderExe = () => path.join(resourcesDir(), 'bin', process.platform === 'win32' ? 'plate-reader.exe' : 'plate-reader');
const plateModelsDir = () => path.join(resourcesDir(), 'plate-models');
const ffmpegExe = () => path.join(resourcesDir(), 'bin', process.platform === 'win32' ? 'ffmpeg.exe' : 'ffmpeg');

module.exports = { projectRoot, resourcesDir, userDataDir, ensureDir, modelPath, plateReaderExe, plateModelsDir, ffmpegExe };
